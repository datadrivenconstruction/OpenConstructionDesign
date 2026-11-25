//! Object scripts kept as scenario fixtures: what a scripted assistant writes in the replay
//! and eval tests (towers, wheels, bridges, roofs). Compiled into test binaries only, never
//! listed to the assistant; `ddc_script::recipes` holds the technique examples it reads.
#![allow(dead_code)]

macro_rules! scenes {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_str!(concat!("scenes/", $name, ".lua")))),*]
    };
}

/// (name, script); the first script line is a `-- summary`.
pub const SCENES: &[(&str, &str)] = scenes!(
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

/// The script of a scene by name.
pub fn scene(name: &str) -> Option<&'static str> {
    SCENES.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}
