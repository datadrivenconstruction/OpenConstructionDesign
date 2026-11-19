//! Every G case against a scripted build: where a recipe, the generator or a short script can
//! answer the brief, a scripted model runs it through the real session, the plan is applied and
//! the case's own oracles judge it. The rest wait for a recipe on another branch or a live model.

use super::case::{evals_dir, Case};
use super::metrics::Status;
use super::run::{replay_profile, run_case, Made, Opts};
use ddc_ai::fake::{Script, ScriptedProvider};
use serde_json::{json, Value as J};
use std::time::Duration;

fn tool(name: &str, args: J) -> Vec<Script> {
    vec![Script::Tool { name: name.into(), args }]
}

/// Runs `id` with the scripted answer `steps`: None when every oracle passes, else why not.
fn judged(id: &str, mut steps: Vec<Vec<Script>>) -> Option<String> {
    let case = Case::load(&evals_dir().join("cases").join(format!("{id}.toml"))).unwrap_or_else(|e| panic!("{e}"));
    steps.push(vec![Script::Text("Done.".into())]);
    let made = Made { provider: ScriptedProvider::new(steps), replay: None, rec: None };
    let o = Opts { out: None, profile: replay_profile(), price_in: 0.0, price_out: 0.0, turn_timeout: Duration::from_secs(300) };
    let m = run_case(&case, &made, &o, None);
    let failed: Vec<String> = m.oracles.iter().filter(|r| !r.pass).map(|r| format!("{}: want {}, got {}", r.name, r.expected, r.measured)).collect();
    (m.status != Status::Pass).then(|| format!("{id}: {:?} {} refusals {:?} failed {failed:#?}", m.status, m.note, m.refusals))
}

fn site() -> Vec<Script> {
    tool("create_level", json!({"title": "Site", "name": "Site", "elevation": 0.0}))
}

/// How a case is built without a live model.
enum Build {
    /// A shipped recipe with these params, on a fresh site level.
    Recipe(&'static str, fn() -> J),
    /// A hand-written answer.
    Steps(fn() -> Vec<Vec<Script>>),
    /// A recipe that lands with another branch (name, branch): runs once it is here.
    Pending(&'static str, &'static str),
    /// Only a live model builds it, and why.
    Live(&'static str),
}
use Build::*;

fn none() -> J {
    json!({})
}

const LIVE: &str = "no recipe or generator path: a live model builds it";
const SPIRAL: &str = "a spiral stair item lands with gen/spiral1";

/// Every G case (base file id) and how it is built.
const BUILDS: &[(&str, Build)] = &[
    ("G01_spiral_stair", Live(SPIRAL)),
    ("G02_ribbed_dome", Live(LIVE)),
    ("G03_gridshell_canopy", Recipe("timber_gridshell", none)),
    ("G04_hypar_roof", Recipe("hypar_umbrellas", none)),
    ("G05_twisted_tower", Recipe("twisted_diagrid_tower", none)),
    ("G06_cable_net", Live(LIVE)),
    ("G07_vaulted_hall", Recipe("pointed_vault_cloister", none)),
    ("G08_arch_bridge", Recipe("tied_arch_bridge", none)),
    ("G09_ship_hull", Live(LIVE)),
    ("G10_chair", Live(LIVE)),
    ("G11_lamp", Live(LIVE)),
    ("G12_sweep_sculpture", Live(LIVE)),
    ("G13_terrain_retaining", Live(LIVE)),
    ("G14_pipe_rack", Live(LIVE)),
    ("G15_tank_nozzles", Live(LIVE)),
    ("G16_fins_facade", Recipe("fin_facade", none)),
    ("G17_voronoi_screen", Recipe("voronoi_screen", none)),
    ("G18_fractal_tree", Live(LIVE)),
    ("G19_stadium_bowl", Recipe("stadium_roof", none)),
    ("G20_amphitheatre", Steps(amphitheatre)),
    ("G21_minaret_tower", Live(SPIRAL)),
    ("G22_pagoda", Pending("pagoda_tiers", "gen/pagoda1")),
    ("G23_geodesic_sphere", Recipe("sphere_pavilion", none)),
    ("G24_mobius_pavilion", Steps(moebius)),
    ("G25_wind_turbine", Pending("wind_turbine", "gen/disp2")),
    ("G26_ferris_wheel", Pending("ferris_wheel", "gen/disp2")),
    ("G27_pylon", Live(LIVE)),
    ("G28_cooling_tower", Live(LIVE)),
    ("G29_helical_ramp", Recipe("observation_ramp_tower", none)),
    ("G30_blob_museum", Live(LIVE)),
    ("G31_suspension_footbridge", Live(LIVE)),
    ("G32_guyed_mast", Live(LIVE)),
    ("G33_lighthouse", Live(LIVE)),
    ("G34_water_tower", Live(LIVE)),
    ("G35_ski_jump", Live(LIVE)),
    ("G36_grain_silos", Live(LIVE)),
    ("G37_arch_hangar", Live(LIVE)),
    ("G38_tensile_sail", Recipe("membrane_canopy", none)),
    ("G39_radio_dish", Live(LIVE)),
    ("G40_stepped_pyramid", Steps(pyramid)),
];

fn recipe_steps(name: &str, params: J) -> Option<Vec<Vec<Script>>> {
    let script = ddc_script::recipes::recipe(name)?;
    Some(vec![site(), tool("run_script", json!({"title": name.replace('_', " "), "script": script, "params": params}))])
}

/// Base G case ids (no language suffix), sorted.
fn g_ids() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(evals_dir().join("cases"))
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter_map(|n| n.strip_suffix(".toml").map(str::to_string))
        .filter(|n| n.starts_with('G') && n.as_bytes().get(1).is_some_and(u8::is_ascii_digit) && !n.contains("__"))
        .collect();
    v.sort();
    v
}

/// A new G case must say how it is built; a stale row must go.
#[test]
fn every_g_case_says_how_it_is_built() {
    let have: Vec<&str> = BUILDS.iter().map(|b| b.0).collect();
    assert_eq!(g_ids().iter().map(String::as_str).collect::<Vec<_>>(), have);
    for (id, b) in BUILDS {
        if let Recipe(name, _) = b {
            assert!(ddc_script::recipes::recipe(name).is_some(), "{id}: no recipe {name}");
        }
    }
}

#[test]
fn every_g_case_with_a_scripted_build_passes_its_oracles() {
    let mut bad = vec![];
    for (id, b) in BUILDS {
        let steps = match b {
            Recipe(name, params) => recipe_steps(name, params()).expect("checked by every_g_case_says_how_it_is_built"),
            Steps(f) => f(),
            Pending(..) | Live(_) => continue,
        };
        bad.extend(judged(id, steps));
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
#[ignore = "recipes from gen/pagoda1 and gen/disp2 are not on this branch yet"]
fn g_cases_with_a_pending_recipe_pass_once_it_lands() {
    let mut bad = vec![];
    for (id, b) in BUILDS {
        let Pending(name, branch) = b else { continue };
        match recipe_steps(name, none()) {
            Some(steps) => bad.extend(judged(id, steps)),
            None => bad.push(format!("{id}: recipe {name} is still on {branch}")),
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// G40: seven 4 m storeys, each plate set back 4 m all round (the generator's step rule).
fn pyramid() -> Vec<Vec<Script>> {
    let spec = json!({
        "typology": "office", "floors": 7, "floor_height": 4.0,
        "footprint": {"shape": "rectangle", "width": 60, "depth": 60},
        "rules": {"massing.step_every": 1, "massing.step_setback": 4}
    });
    vec![tool("generate_project", json!({"spec": spec}))]
}

/// G24: a Moebius ribbon 3 m wide on an 18 m loop, a rib about every metre, one half twist.
fn moebius() -> Vec<Vec<Script>> {
    let script = "\
local function band(u, v)
  local th, s = 2 * math.pi * u, (v - 0.5) * 3
  local r = 9 + s * math.cos(th / 2)
  return vec3(r * math.cos(th), r * math.sin(th), 4.5 + s * math.sin(th / 2))
end
surface_grid{surface = band, nu = 56, nv = 3, closed_u = true, flip_seam = true, name = 'ribbon'}
audit{strict = true}
";
    vec![site(), tool("run_script", json!({"title": "Moebius pavilion", "script": script, "params": {}}))]
}

/// G20: twenty stepped seat rows on a half circle of 30 m, a stage roof on four columns.
fn amphitheatre() -> Vec<Vec<Script>> {
    let script = "\
for i = 0, 19 do
  local r, z = 10 + i * 1, i * 0.45
  for k = 0, 11 do
    local a, b = math.pi * k / 12, math.pi * (k + 1) / 12
    part('seat', extrude(profile.rect(0.9, 0.45), vec3(r * math.cos(a), r * math.sin(a), z),
                         vec3(r * math.cos(b), r * math.sin(b), z)), 'concrete')
  end
end
";
    let posts: Vec<J> = [[-6.0, -2.0], [6.0, -2.0], [-6.0, -8.0], [6.0, -8.0]]
        .iter()
        .map(|p| json!({"at": p, "level": "Site", "top_offset": 6.0}))
        .collect();
    vec![
        site(),
        tool("run_script", json!({"title": "Seat rows", "script": script, "params": {}})),
        tool("create_elements", json!({"title": "Stage columns", "cat": "column", "items": posts})),
    ]
}
