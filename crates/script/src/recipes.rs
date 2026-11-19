// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Worked scripts for freeform structures, one per block family, and a short reference of
//! the blocks. Shown to the assistant by the `script_recipes` tool.

macro_rules! recipes {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_str!(concat!("../data/recipes/", $name, ".lua")))),*]
    };
}

/// (name, script); the first script line is a `-- summary`.
pub const RECIPES: &[(&str, &str)] = recipes!(
    "twisted_diagrid_tower",
    "timber_gridshell",
    "cable_stayed_footbridge",
    "stadium_roof",
    "observation_ramp_tower",
    "ruled_shell_hall",
    "fin_facade",
    "catenary_market",
    "sphere_pavilion",
    "hypar_umbrellas",
    "membrane_canopy",
    "space_frame_hall",
    "hangar_space_frame",
    "tied_arch_bridge",
    "truss_footbridge",
    "tapered_tower",
    "voronoi_screen",
    "sun_louvres",
    "spiral_loft_pavilion",
    "pointed_vault_cloister",
    "wave_canopy",
    "pagoda_tiers",
    "wind_turbine",
    "ferris_wheel",
);

/// The summary line of a recipe.
pub fn summary(script: &str) -> &str {
    script.lines().next().unwrap_or("").trim_start_matches("--").trim()
}

/// The script of a recipe by name.
pub fn recipe(name: &str) -> Option<&'static str> {
    RECIPES.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

/// The `param(...)` declarations of a script, one string each, so a reader sees what a recipe
/// lets the person change (name, default, bounds) without reading the whole script.
pub fn params_of(script: &str) -> Vec<String> {
    script
        .lines()
        .filter_map(|l| {
            let at = l.find("param(")?;
            let tail = &l[at..];
            let mut depth = 0i32;
            for (i, ch) in tail.char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(tail[..=i].to_string());
                        }
                    }
                    _ => {}
                }
            }
            None
        })
        .collect()
}

/// The reference lines of the blocks a name stands for: `tower`, `bridge`, `surf.dome`, `arch`.
/// A name matches whole identifiers only, so `umbrella` does not find `hypar_umbrella`.
pub fn block_lines(name: &str) -> Vec<&'static str> {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let name = name.trim();
    if name.is_empty() {
        return vec![];
    }
    BLOCKS
        .lines()
        .filter(|l| {
            l.match_indices(name).any(|(i, _)| {
                let before = l[..i].chars().next_back();
                let after = l[i + name.len()..].chars().next();
                !before.is_some_and(ident)
                    && after.is_some_and(|c| matches!(c, '{' | '|' | '.' | ','))
            })
        })
        .collect()
}

/// One line per block: call and main options. Lengths take numbers (m) or "300 mm"; angles
/// take radians or "30 deg"; profiles take "CHS 508x20", "RHS 300x200x10", "SHS 200x10",
/// "rect 120x240", "round 40", "I 600x300" or a catalogue name. Every block returns
/// (group, {nodes, counts, parts}) and refuses unknown options.
pub const BLOCKS: &str = "\
tower{floors, floor_h, radius, sides, twist, taper, system=diagrid|columns, module, slab_t, core_r (0 = none), core_t, diagonal, column, beam, center}
space_frame{nx, ny, bay, depth, height, columns=corners|perimeter|none, column_every, chord, bottom, diagonal, column, deck, crane_z (runway beams + crane girder), gates (sliding leaves), gate_h, header, origin, angle, size, rules}
arch{from, to, kind=catenary|parabolic|circular|pointed, rise, segments, profile, role}
vault{span, length, kind, rise, arches, segments, rib, purlin, panels (thickness, 0 = none), origin, angle}
helix_stair{center, height, r_out, going 0.22-0.40, riser_max <= 0.20, column, clockwise, start}
helix_ramp{center, height, r_in, width, slope_max, segments (per turn), column_every, edge, cross, deck, railing}
fins{from, to, height, direction=vertical|horizontal, spacing, depth, depth_min + waves (sine swing of the depth along the run), thickness, mode=attractor|sun|fixed, attractor, sun={azimuth, altitude}, min_rotation, max_rotation}
bridge{kind=girder|truss|arch|cable_stayed, span, width, deck_z, bays, depth, rise, pylon_h, pylon_at, piers (interior), ramp_slope (0.05 = 1:20 approach ramps), ramp_bay, girder, cross, chord, web, arch, hanger, stay, pylon, pier, wind_brace, wind_bracing, footbridge, origin, angle, railing, size, rules}
stadium_roof{a, b (opening half-axes), depth, backspan (back to front column, >= a quarter of the cantilever; 0 = first panel node), bays, divisions, height, truss_depth, front_depth, rake, cover, backstay, stay_every, anchor, bowl (three raked seating tiers), inset, chord, web, ring, purlin, column, stay, size, rules}
surf.hypar|bilinear{corners}, surf.dome{radius, rise, oculus}, surf.vault{span, length, kind, rise}, surf.loft{curves}, surf.ruled{a, b}, surf.revolve{profile = {vec3(r, 0, z)...}, from, to}, surf.wave{size_x, size_y, height, amplitude, waves_x, waves_y}; s(u, v) -> vec3
surface_grid{surface (surf.* or function(u, v)), nu, nv, pattern=quad|tri|diagrid, closed_u, flip_seam (closed_u with a half twist: Moebius), member, edge, panels, columns = {{u, v}...}, edge_columns (every n-th edge node), ground}
geodesic{center, radius, frequency 1-8, cut (-1 sphere, 0 hemisphere; odd frequency: zigzag rim), member, edge (open rim), panels, panel_material}
hypar_umbrella{origin, size, height, rise, nx, ny, divisions, rib, member, column, shell}
pagoda{origin, tiers 1-15, base_width, tier_height, taper (tier width ratio), overhang, eave_curl (corner lift of the eaves), roof_slope, roof_t, floor, finial, column, beam, rafter, mast, material, roof_material}
membrane{corners = {vec3...}, peak, rings, divisions, thickness, mast, cable, guys}
perforated_panel{from, to, count, height, base, cells, seed, rib, border, thickness, attractor, open_min, post}
bridge, stadium_roof and space_frame size their members by a frame analysis (size = false skips it; a profile option fixes that group; rules = {util_max = 0.8, fy = 460, live_deck = 4, ...} steers it) and return info.check = {ok, util_max, deflection_mm, limit_mm, span_depth, f1_hz, mass_t, sections, why = {reasons}, notes}
audit{strict, ground}: node misses, duplicate and clashing members, floating groups; strict = error";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_is_found_by_its_name_and_only_by_whole_names() {
        assert!(block_lines("tower")[0].starts_with("tower{"));
        assert!(block_lines("bridge")[0].starts_with("bridge{"));
        assert!(block_lines("space_frame")[0].starts_with("space_frame{"));
        assert!(block_lines("surf.dome")[0].contains("surf.dome{"));
        assert!(block_lines("hypar_umbrella")[0].starts_with("hypar_umbrella{"));
        assert!(block_lines("umbrella").is_empty());
        assert!(block_lines("").is_empty());
    }

    #[test]
    fn every_recipe_lists_its_params_with_bounds() {
        for (name, script) in RECIPES {
            assert!(!summary(script).is_empty(), "{name}");
        }
        let p = params_of(recipe("tied_arch_bridge").unwrap());
        assert!(p.iter().any(|d| d.starts_with("param(\"span\"") && d.contains("max")), "{p:?}");
    }
}
