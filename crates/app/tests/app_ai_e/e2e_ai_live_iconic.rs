// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Twenty landmark structures the assistant builds with script blocks: briefs (en, ru), the
//! recipe a good answer adapts, and acceptance read back from the applied model (parts by
//! role, overall size, realism errors 0). Scripted replays run the recipes through the real
//! session; the briefs are exported for the live harness and its output is checked here.
//!
//! Export briefs: DDC_ICONIC_TASKS=tasks.json cargo test -p ddc-app --test app_ai_e e2e_ai_live_iconic::export -- --ignored
//! Check live output: DDC_LIVE_OUT=dir cargo test ... live_output -- --ignored
//! Pictures: DDC_ICONIC_SHOTS=dir with the replays (needs a graphics adapter).

use crate::{e2e_ai_common, e2e_common};

use ddc_app::editor::Editor;
use ddc_model::checks::{realism, CheckScope, Severity};
use e2e_ai_common::*;
use e2e_common::count;
use serde_json::{json, Value as J};

struct Scn {
    id: &'static str,
    recipe: &'static str,
    en: &'static str,
    /// Minimum parts per role (the first word of a solid's name), for the replay.
    roles: &'static [(&'static str, i64)],
    /// Minimum overall size x, y, z in metres (the plan may be turned: x and y sorted).
    size: [f64; 3],
    parts: i64,
}

const SCNS: &[Scn] = &[
    Scn { id: "X01", recipe: "twisted_diagrid_tower",
        en: "Design a 24-storey twisted tower about 96 m tall: a steel diagrid of tubes in staggered rows that turns 90 degrees from base to roof and tapers to 80 % of its 32 m base diameter. Every floor has a slab on ring beams round a concrete core.",
        roles: &[("diagonal", 288), ("slab", 24), ("core", 24), ("ring_beam", 400)], size: [25.0, 25.0, 95.0], parts: 700 },
    Scn { id: "X02", recipe: "timber_gridshell",
        en: "Build a timber gridshell pavilion: a shallow dome 36 m across and 7 m high with a 6 m oculus, glulam laths in a diagonal grid and a stiffer edge ring on the ground.",
        roles: &[("diagonal", 200), ("edge", 60)], size: [34.0, 34.0, 6.0], parts: 260 },
    Scn { id: "X03", recipe: "cable_stayed_footbridge",
        en: "Design an 80 m cable-stayed footbridge, deck 4 m wide at 6 m above ground. One H-shaped pylon 36 m tall stands off-centre and fan stays carry both edge girders; add deck slabs, cross beams, end piers and railings.",
        roles: &[("pylon", 4), ("stay", 20), ("deck", 16), ("girder", 32)], size: [79.0, 5.0, 35.0], parts: 150 },
    Scn { id: "X04", recipe: "stadium_roof",
        en: "Roof a stadium bowl: an elliptic opening of 120 by 84 m, 24 m deep cantilever trusses at 28 m height on two columns each, purlins, a front compression ring and translucent cover panels.",
        roles: &[("column", 64), ("cover", 160), ("top_chord", 160), ("front_ring", 32)], size: [160.0, 125.0, 28.0], parts: 1000 },
    Scn { id: "X05", recipe: "observation_ramp_tower",
        en: "Make a 20 m observation tower with an accessible helical ramp at 1:12, 2.4 m wide, wound round a steel frame with landings every 4 m, railings on the outer edge and columns under the ramp.",
        roles: &[("deck", 100), ("handrail", 100), ("slab", 5), ("column", 40)], size: [16.0, 16.0, 20.0], parts: 800 },
    Scn { id: "X06", recipe: "ruled_shell_hall",
        en: "Design a 40 by 20 m hall roofed by a ruled surface: straight rulings from a level eave at 6 m to a ridge that waves between 8 and 12 m, timber panels on a steel grid, columns along both long edges.",
        roles: &[("panel", 160), ("column", 20), ("u_rib", 100)], size: [39.0, 19.0, 11.0], parts: 400 },
    Scn { id: "X07", recipe: "fin_facade",
        en: "Put a kinetic-looking fin screen on a 36 m long, 16 m high facade: vertical aluminium fins every 0.75 m, turned more the closer they are to a point in front of the entrance, between steel frame rails.",
        roles: &[("fin", 48), ("rail", 2)], size: [36.0, 0.3, 16.0], parts: 50 },
    Scn { id: "X08", recipe: "catenary_market",
        en: "Design a covered market 24 m wide and 48 m long under catenary steel arches 12 m high, with purlins and glazing between them.",
        roles: &[("rib", 144), ("panel", 128), ("purlin", 100)], size: [23.0, 47.0, 11.5], parts: 400 },
    Scn { id: "X09", recipe: "sphere_pavilion",
        en: "Build a sphere pavilion about 15 m across standing on a small ring, made of triangulated steel tubes with an opening at the top.",
        roles: &[("diagonal", 250), ("u_rib", 200)], size: [14.0, 14.0, 13.5], parts: 700 },
    Scn { id: "X10", recipe: "hypar_umbrellas",
        en: "Cover a 30 by 20 m market with six inverted umbrellas, each 10 m square on one column 6 m high and made of four hyperbolic-paraboloid shell quadrants with straight ribs.",
        roles: &[("column", 6), ("shell", 600), ("rib", 150)], size: [29.0, 19.0, 7.5], parts: 1000 },
    Scn { id: "X11", recipe: "membrane_canopy",
        en: "Make a 16 by 16 m tensile membrane sail with two high points at 7 m and two low points at 3 m, edge cables, a mast with a guy at every corner.",
        roles: &[("mast", 4), ("guy", 4), ("membrane", 190), ("edge_cable", 24)], size: [16.0, 16.0, 7.0], parts: 220 },
    Scn { id: "X12", recipe: "space_frame_hall",
        en: "Design an exhibition hall roofed by a 36 by 24 m square-on-square space frame at 9 m, on perimeter columns every 9 m.",
        roles: &[("diagonal", 384), ("top_chord", 200), ("column", 10)], size: [33.0, 21.0, 9.0], parts: 750 },
    Scn { id: "X13", recipe: "tied_arch_bridge",
        en: "Build a 60 m tied-arch road bridge, 8 m wide, deck at 5 m: two parabolic arches 12 m high above the girders, hangers at every bay and bracing between the arches.",
        roles: &[("arch", 24), ("hanger", 22), ("deck", 12), ("arch_brace", 5)], size: [59.0, 8.0, 16.5], parts: 150 },
    Scn { id: "X14", recipe: "truss_footbridge",
        en: "Design a 36 m Pratt truss footbridge, 3.5 m wide and 3 m deep, deck at 4 m, with top bracing and railings.",
        roles: &[("top_chord", 18), ("diagonal", 18), ("vertical", 20), ("top_brace", 10)], size: [35.0, 3.5, 7.0], parts: 120 },
    Scn { id: "X15", recipe: "tapered_tower",
        en: "Make a 30-storey tower 108 m tall whose ten inclined perimeter columns turn 45 degrees and lean in to 60 % of the 30 m base diameter, with slabs round a core.",
        roles: &[("column", 300), ("slab", 30), ("core", 30)], size: [27.0, 27.0, 107.0], parts: 650 },
    Scn { id: "X16", recipe: "voronoi_screen",
        en: "Make a 14.4 m garden screen of four perforated weathering-steel plates with Voronoi openings, larger near the gate in the middle, between square posts.",
        roles: &[("plate", 4), ("post", 5)], size: [14.0, 0.1, 3.2], parts: 9 },
    Scn { id: "X17", recipe: "sun_louvres",
        en: "Add sun louvres in front of a 12 m wide, 6 m high south glazing: horizontal blades every 0.4 m tilted for a sun at 50 degrees, between two posts.",
        roles: &[("louvre", 14), ("post", 2)], size: [12.0, 0.2, 6.0], parts: 16 },
    Scn { id: "X18", recipe: "spiral_loft_pavilion",
        en: "Design a 12 m pavilion whose shell is lofted through elliptic sections that swell at mid-height and turn 60 degrees in total, as a diagrid of steel tubes.",
        roles: &[("diagonal", 144), ("edge", 48)], size: [17.0, 12.0, 12.0], parts: 190 },
    Scn { id: "X19", recipe: "pointed_vault_cloister",
        en: "Build a 40 m cloister walk: a glazed pointed vault 8 m wide and 7 m high on steel ribs, and beside it an arcade of free-standing pointed arches every 4 m.",
        roles: &[("rib", 200), ("panel", 120), ("purlin", 100)], size: [13.0, 40.0, 7.0], parts: 450 },
    Scn { id: "X20", recipe: "wave_canopy",
        en: "Cover a 40 by 16 m plaza with a glazed wave canopy about 7 m high, rising and falling 2 m, as a triangulated steel gridshell on columns along both long edges.",
        roles: &[("panel", 320), ("column", 10), ("diagonal", 160)], size: [39.0, 15.0, 8.5], parts: 600 },
];

/// The Russian brief of a scenario, kept in a data file keyed by scenario id.
fn ru_brief(id: &str) -> String {
    static DATA: &str = include_str!("../data/app2_iconic_ru.json");
    let m: std::collections::HashMap<String, String> =
        serde_json::from_str(DATA).expect("app2_iconic_ru.json");
    m.get(id).unwrap_or_else(|| panic!("no Russian brief for {id}")).clone()
}

fn scn(id: &str) -> &'static Scn {
    SCNS.iter().find(|s| s.id == id).expect("scenario")
}

fn roles(ed: &Editor) -> Vec<(String, i64)> {
    let db = ed.model.sql();
    let mut st = db
        .prepare(
            "SELECT CASE WHEN instr(name, ' ') > 0 THEN substr(name, 1, instr(name, ' ') - 1) \
             ELSE name END AS r, count(*) FROM solid GROUP BY r ORDER BY r",
        )
        .unwrap();
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|r| r.unwrap()).collect()
}

/// Overall size of the solids, x and y sorted so a turned plan still compares.
fn size(ed: &Editor) -> [f64; 3] {
    let b: [f64; 6] = ed
        .model
        .sql()
        .query_row(
            "SELECT min(x0), min(y0), min(z0), max(x1), max(y1), max(z1) FROM solid_qty",
            [],
            |r| Ok([r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?]),
        )
        .unwrap();
    let (dx, dy) = (b[3] - b[0], b[4] - b[1]);
    [dx.max(dy), dx.min(dy), b[5] - b[2]]
}

/// What every accepted model satisfies: enough parts, the size asked, no realism errors,
/// no failed solids. Returns the problems found.
fn accept(ed: &Editor, s: &Scn, by_role: bool) -> Vec<String> {
    let mut bad = vec![];
    let n = count(&ed.model, "solid");
    let need = if by_role { s.parts } else { s.parts / 4 };
    if n < need {
        bad.push(format!("{n} solids, expected at least {need}"));
    }
    let failed: i64 = ed
        .model
        .sql()
        .query_row("SELECT count(*) FROM solid_qty WHERE error IS NOT NULL", [], |r| r.get(0))
        .unwrap();
    if failed > 0 {
        bad.push(format!("{failed} solids failed to build"));
    }
    let got = size(ed);
    let mut want = s.size;
    want[..2].sort_by(|a, b| b.total_cmp(a));
    let slack = if by_role { 1.0 } else { 0.7 };
    for k in 0..3 {
        if got[k] + 1e-6 < want[k] * slack {
            bad.push(format!("size {got:.1?} below {want:?}"));
            break;
        }
    }
    let errors: Vec<String> = realism::critique(&ed.model, &CheckScope::All)
        .unwrap()
        .into_iter()
        .filter(|f| f.severity == Severity::Error)
        .map(|f| format!("{} {:?}", f.msg_key, f.args))
        .collect();
    if !errors.is_empty() {
        bad.push(format!("{} realism errors: {:?}", errors.len(), &errors[..errors.len().min(5)]));
    }
    if by_role {
        let have = roles(ed);
        for (r, min) in s.roles {
            let n = have.iter().find(|h| h.0 == *r).map_or(0, |h| h.1);
            if n < *min {
                bad.push(format!("{n} {r}, expected at least {min}"));
            }
        }
    }
    bad
}

fn shot(ed: &Editor, name: &str) {
    let Ok(dir) = std::env::var("DDC_ICONIC_SHOTS") else { return };
    std::fs::create_dir_all(&dir).unwrap();
    for (k, (yaw, pitch)) in [(35.0, 25.0), (-60.0, 12.0)].iter().enumerate() {
        let req = ddc_app::ai::render_req::parse_3d(&json!({"yaw": yaw, "pitch": pitch, "max_px": 1400}))
            .unwrap();
        match ddc_app::ai::render_off::view3d(ed, ed.tool.units, &req) {
            Ok(r) => {
                let ext = if r.out.image.mime.contains("png") { "png" } else { "jpg" };
                let p = std::path::Path::new(&dir).join(format!("{name}_{k}.{ext}"));
                std::fs::write(p, r.out.image.bytes.as_slice()).unwrap();
            }
            Err(e) => eprintln!("{name}: no picture ({e})"),
        }
    }
}

/// The blocks a script calls (`name{`).
fn block_calls(script: &str) -> Vec<&str> {
    script
        .match_indices('{')
        .filter_map(|(i, _)| {
            let head = &script[..i];
            let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '.';
            let start = head.rfind(|c: char| !ident(c)).map_or(0, |k| k + 1);
            let name = &head[start..];
            (!name.is_empty()).then_some(name)
        })
        .collect()
}

/// The worked technique a good answer opens first: one that calls a block the scene calls.
fn technique_for(script: &str) -> &'static str {
    let mine = block_calls(script);
    let all = ddc_script::recipes::RECIPES;
    let shares = |t: &str| block_calls(t).iter().any(|b| mine.contains(b));
    all.iter().find(|(_, t)| shares(t)).unwrap_or(&all[0]).0
}

/// The scripted assistant reads the block reference, opens the nearest technique example and
/// runs its own script for the object (the scene); the plan is applied and the model must
/// pass the scenario.
fn replay(id: &str) {
    let s = scn(id);
    let mut ed = Editor::new_in_memory().unwrap();
    add_level(&mut ed, "Ground", 0.0);
    let script = crate::scenes::scene(s.recipe).expect("scene");
    let technique = technique_for(script);
    let (mut session, provider) = run(
        &mut ed,
        vec![
            tool("script_recipes", json!({})),
            tool("script_recipes", json!({"name": technique})),
            tool("run_script", json!({"title": s.recipe.replace('_', " "), "script": script, "params": {}})),
        ],
    );
    let results = tool_results(&provider);
    assert_eq!(results.len(), 3, "{results:?}");
    assert!(results.iter().all(|r| !r.1), "{id}: {results:?}");
    let list = parsed(&results[0].0);
    // The tool lists the whole technique table, no more and no less, and no object scene.
    assert_eq!(list["recipes"].as_array().map(Vec::len), Some(ddc_script::recipes::RECIPES.len()), "{list}");
    assert!((3..=6).contains(&ddc_script::recipes::RECIPES.len()), "a handful of techniques");
    let listed = ddc_script::recipes::recipe(s.recipe);
    assert!(listed.is_none(), "{}: an object script is listed", s.recipe);
    assert!(list["blocks"].as_str().is_some_and(|b| b.contains("surface_grid{")));
    let one = parsed(&results[1].0);
    assert_eq!(one["script"].as_str(), ddc_script::recipes::recipe(technique));
    let run_r = parsed(&results[2].0);
    assert!(run_r["changes"]["created"].as_i64().unwrap_or(0) >= s.parts, "{id}: {run_r}");
    apply_and_check(&mut session, &mut ed);
    drop(session);
    let bad = accept(&ed, s, true);
    assert!(bad.is_empty(), "{id} {}: {bad:?}", s.recipe);
    shot(&ed, &format!("{id}_{}", s.recipe));
}

#[test]
fn x01_twisted_diagrid_tower() {
    replay("X01");
}

#[test]
fn x02_timber_gridshell() {
    replay("X02");
}

#[test]
fn x03_cable_stayed_footbridge() {
    replay("X03");
}

#[test]
fn x07_fin_facade() {
    replay("X07");
}

#[test]
fn x08_catenary_market() {
    replay("X08");
}

#[test]
fn x10_hypar_umbrellas() {
    replay("X10");
}

#[test]
fn x11_membrane_canopy() {
    replay("X11");
}

#[test]
fn x13_tied_arch_bridge() {
    replay("X13");
}

#[test]
fn x16_voronoi_screen() {
    replay("X16");
}

#[test]
fn x19_pointed_vault_cloister() {
    replay("X19");
}

#[test]
#[ignore = "all twenty replays, about ten minutes in debug"]
fn all_twenty_replays() {
    for s in SCNS {
        replay(s.id);
    }
}

#[test]
fn scenarios_are_complete() {
    assert_eq!(SCNS.len(), 20);
    for s in SCNS {
        assert!(crate::scenes::scene(s.recipe).is_some(), "{}", s.id);
        assert!(s.en.len() > 80 && ru_brief(s.id).len() > 80, "{}: briefs", s.id);
        assert!(!s.roles.is_empty() && s.size.iter().all(|v| *v > 0.0));
    }
}

#[test]
#[ignore = "writes the briefs for the live harness to DDC_ICONIC_TASKS"]
fn export_live_tasks() {
    let Ok(path) = std::env::var("DDC_ICONIC_TASKS") else { return };
    let mut tasks: Vec<J> = vec![];
    for s in SCNS {
        tasks.push(json!({"id": format!("{}_en", s.id), "brief": s.en}));
        tasks.push(json!({"id": format!("{}_ru", s.id), "brief": ru_brief(s.id)}));
    }
    std::fs::write(path, serde_json::to_string_pretty(&tasks).unwrap()).unwrap();
}

#[test]
#[ignore = "checks the projects a live run saved in DDC_LIVE_OUT"]
fn check_live_output() {
    let Ok(out) = std::env::var("DDC_LIVE_OUT") else { return };
    let mut report = vec![];
    for s in SCNS {
        for lang in ["en", "ru"] {
            let id = format!("{}_{lang}", s.id);
            let file = std::path::Path::new(&out).join(format!("{id}.ddc"));
            if !file.exists() {
                report.push(json!({"id": id, "ok": false, "problems": ["no saved project"]}));
                continue;
            }
            let mut ed = Editor::new_in_memory().unwrap();
            ed.open_project(&file).unwrap();
            let bad = accept(&ed, s, false);
            shot(&ed, &id);
            report.push(json!({"id": id, "ok": bad.is_empty(), "problems": bad, "solids": count(&ed.model, "solid")}));
        }
    }
    let passed = report.iter().filter(|r| r["ok"] == json!(true)).count();
    let text = serde_json::to_string_pretty(&report).unwrap();
    std::fs::write(std::path::Path::new(&out).join("iconic_report.json"), &text).unwrap();
    eprintln!("iconic: {passed} of {} accepted", report.len());
}
