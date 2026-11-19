// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! 3D scene element ids (`SceneMesh::element`, u32) and category codes `Sel::Ext`/`Cat::Ext`
//! (u16) — a single source for all modules. Ranges do not overlap (checked at build time):
//! editor lists at the bottom; shapes; stable `mep3d` ids (architecture, networks, components);
//! `edit3d::scene3d` provider parts; terrain; service ids (`OTHER_LEVEL`, nodes) near `u32::MAX`.

/// Length of each stable range.
pub const SPAN: u32 = 0x0800_0000;
pub const MEP_SPAN: u32 = SPAN;
/// Editor ids (`Editor::scene_all`, position in its lists): `0..EDITOR_END`.
pub const EDITOR_END: u32 = MASS_BASE;
/// Shapes: one block per shape, a face is an element (`mass_tools`).
pub const MASS_BASE: u32 = 0x4000_0000;
pub const MASS_END: u32 = ARCH_BASE;
/// Stable `mep3d` ids: architecture, network segments, components.
pub const ARCH_BASE: u32 = 0x4800_0000;
pub const MEP_BASE: u32 = ARCH_BASE + SPAN;
pub const COMP_BASE: u32 = MEP_BASE + SPAN;
/// Scene provider parts (`edit3d::scene3d`): `EXT_BASE..=EXT_END`.
pub const EXT_BASE: u32 = COMP_BASE + SPAN;
pub const EXT_END: u32 = EXT_BASE + SPAN - 1;
/// Terrain: the k-th terrain is `TOPO_BASE + k`, at most `TOPO_MAX`.
pub const TOPO_BASE: u32 = u32::MAX - 0x1_0000;
pub const TOPO_MAX: u32 = 0xF000;
/// Start of service ids: everything above is not an element.
pub const SERVICE_BASE: u32 = u32::MAX - 15;
pub const RESERVED_TOP: u32 = SERVICE_BASE;
/// Service ids: other levels (not selectable), other editor bodies, the renderer's "no pick".
/// Site surroundings bodies (terrain, import pads): drawn, but not part of "show all".
pub const SITE: u32 = u32::MAX - 4;
/// AI plan projection over the scene: not selectable, not stored in the base scene cache.
pub const AI_PREVIEW: u32 = u32::MAX - 9;
/// Meshes of linked models (`links_ui`): drawn, not selectable.
pub const LINK: u32 = u32::MAX - 7;
pub const OTHER_LEVEL: u32 = u32::MAX - 3;
pub const EDITOR_MISC: u32 = u32::MAX - 1;

/// All ranges `[start, end)` in order.
pub const RANGES: [(&str, u32, u32); 8] = [
    ("editor", 0, EDITOR_END),
    ("mass", MASS_BASE, MASS_END),
    ("arch", ARCH_BASE, ARCH_BASE + SPAN),
    ("mep", MEP_BASE, MEP_BASE + SPAN),
    ("comp", COMP_BASE, COMP_BASE + SPAN),
    ("ext", EXT_BASE, EXT_END + 1),
    ("topo", TOPO_BASE, TOPO_BASE + TOPO_MAX),
    ("service", SERVICE_BASE, u32::MAX),
];

/// `Sel::Ext` / `Cat::Ext` codes of other modules (u16).
/// Typed solids (`solid_edit3d::CAT`).
pub const EXT_SOLID: u16 = 23;
/// Roof edges (`roof_edge_scene::CAT`).
pub const EXT_ROOF_EDGE: u16 = 27;
/// Structural elements: `STRUCT_BASE + index into STRUCT_KINDS`.
pub const EXT_STRUCT_BASE: u16 = 0x1A00;
pub const EXT_STRUCT_LEN: u16 = 6;
/// Reinforcement (bar set) and analytical model (element).
pub const EXT_REBAR: u16 = 0x1B10;
pub const EXT_ANALYTICAL: u16 = 0x1B11;
/// d6 network fittings: the key is `ddc_model::fitting_key`, the model has no row for them.
pub const EXT_FITTING: u16 = 0x1B12;
/// Mass (selected as `Sel::Mass`) and terrain: 3D edit targets with their own scenes.
pub const EXT_MASS: u16 = 0x1B13;
pub const EXT_TOPO: u16 = 0x1B14;
/// Freeform NURBS surfaces and shells (`freeform_edit3d::CAT`).
pub const EXT_FREEFORM: u16 = 0x1B15;
/// Curtain systems on freeform surfaces (`panel_system_edit3d::CAT`).
pub const EXT_PANEL_SYSTEM: u16 = 0x1B16;
/// Roofs and slabs on freeform surfaces (`shell_elem_edit3d::CAT`).
pub const EXT_SURFACE_SHELL: u16 = 0x1B17;
/// Structure built beside `STRUCT_KINDS` (gridshells, steel connections): drawn in 3D by
/// `edit3d::struct3d::BuiltRest`.
pub const EXT_STRUCT_REST: u16 = 0x1B18;
/// Ramp, room separator, zone, roof, shaft: `EXT_CORE_BASE + CORE_KINDS index`.
pub const EXT_CORE_BASE: u16 = 0x2000;

const fn disjoint() -> bool {
    let mut i = 1;
    while i < RANGES.len() {
        if RANGES[i - 1].2 > RANGES[i].1 || RANGES[i].1 >= RANGES[i].2 {
            return false;
        }
        i += 1;
    }
    true
}

const _: () = assert!(disjoint(), "scene id ranges overlap");
const _: () = {
    assert!(OTHER_LEVEL >= SERVICE_BASE && EDITOR_MISC >= SERVICE_BASE);
    assert!(OTHER_LEVEL != EDITOR_MISC && EDITOR_MISC != u32::MAX);
    assert!(SITE >= SERVICE_BASE && SITE != OTHER_LEVEL && SITE != EDITOR_MISC);
    assert!(LINK >= SERVICE_BASE && LINK != SITE && LINK != OTHER_LEVEL && LINK != EDITOR_MISC);
    assert!(AI_PREVIEW >= SERVICE_BASE && AI_PREVIEW != LINK && AI_PREVIEW != SITE);
    assert!(crate::editor::levels::OTHER_LEVEL >= SERVICE_BASE);
    assert!(EXT_ROOF_EDGE < EXT_STRUCT_BASE || EXT_ROOF_EDGE >= EXT_STRUCT_BASE + EXT_STRUCT_LEN);
    assert!(EXT_SOLID < EXT_STRUCT_BASE && EXT_SOLID != EXT_ROOF_EDGE);
    assert!(EXT_REBAR >= EXT_STRUCT_BASE + EXT_STRUCT_LEN && EXT_ANALYTICAL != EXT_REBAR);
    assert!(EXT_FITTING != EXT_REBAR && EXT_FITTING != EXT_ANALYTICAL);
    assert!(EXT_MASS > EXT_FITTING && EXT_TOPO > EXT_MASS && EXT_TOPO < EXT_CORE_BASE);
    assert!(EXT_FREEFORM > EXT_TOPO && EXT_FREEFORM < EXT_CORE_BASE);
    assert!(EXT_PANEL_SYSTEM > EXT_FREEFORM && EXT_PANEL_SYSTEM < EXT_CORE_BASE);
    assert!(EXT_SURFACE_SHELL > EXT_FREEFORM && EXT_SURFACE_SHELL < EXT_CORE_BASE);
    assert!(EXT_STRUCT_REST > EXT_SURFACE_SHELL && EXT_STRUCT_REST < EXT_CORE_BASE);
};

/// Range of an id (name from `RANGES`).
pub fn range_of(e: u32) -> Option<&'static str> {
    RANGES.iter().find(|r| (r.1..r.2).contains(&e)).map(|r| r.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_are_disjoint_and_named() {
        assert!(disjoint());
        assert_eq!(range_of(5), Some("editor"));
        assert_eq!(range_of(MASS_BASE), Some("mass"));
        assert_eq!(range_of(ARCH_BASE), Some("arch"));
        assert_eq!(range_of(MEP_BASE + 1), Some("mep"));
        assert_eq!(range_of(COMP_BASE), Some("comp"));
        assert_eq!(range_of(EXT_BASE), Some("ext"));
        assert_eq!(range_of(EXT_END), Some("ext"));
        assert_eq!(range_of(TOPO_BASE + 1), Some("topo"));
        assert_eq!(range_of(OTHER_LEVEL), Some("service"));
        assert_eq!(range_of(EDITOR_MISC), Some("service"));
        assert_eq!(range_of(SITE), Some("service"));
        // no stable mep3d id falls into the provider parts range
        for e in [ARCH_BASE, MEP_BASE, COMP_BASE, COMP_BASE + SPAN - 1] {
            assert!(!(EXT_BASE..=EXT_END).contains(&e));
        }
    }

    #[test]
    fn struct_kinds_fit_their_ext_range() {
        assert_eq!(ddc_model::STRUCT_KINDS.len(), EXT_STRUCT_LEN as usize);
    }
}
