// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The live briefs that drafted but did not apply: a two-storey villa on a sloping plot and a
//! seven-storey U-shaped mixed-use block. Each drafts, applies in one step, and every
//! wall-hosted component of the model stands within its wall.

use crate::{e2e_ai_common, e2e_common};

use ddc_app::editor::Editor;
use e2e_ai_common::*;
use e2e_common::count;
use serde_json::{json, Value as J};

fn editor() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    ed
}

/// Hosted components past their wall's axis span: (comp, wall, along, wall length).
fn off_wall(ed: &Editor) -> Vec<(i64, i64, f64, f64)> {
    let db = ed.model.sql();
    let mut q = db
        .prepare(
            "SELECT c.id, w.id, c.x, w.ax, w.ay, w.bx, w.by, w.bulge FROM comp c JOIN wall w ON w.id = c.host_wall",
        )
        .unwrap();
    q.query_map([], |r| {
        let (ax, ay, bx, by): (f64, f64, f64, f64) = (r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?);
        let len = ddc_geom::arc::axis_length(ddc_geom::v2(ax, ay), ddc_geom::v2(bx, by), r.get(7)?);
        Ok((r.get(0)?, r.get(1)?, r.get::<_, f64>(2)?, len))
    })
    .unwrap()
    .map(Result::unwrap)
    .filter(|(_, _, x, len)| *x < -1e-9 || *x > len + 1e-9)
    .collect()
}

fn drafts_and_applies(spec: J) -> Editor {
    let mut ed = editor();
    let (mut s, provider) = run(&mut ed, vec![tool("generate_project", json!({"spec": spec}))]);
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{spec}: {results:?}");
    apply_and_check(&mut s, &mut ed);
    assert!(count(&ed.model, "wall") > 0);
    assert!(off_wall(&ed).is_empty(), "{spec}: {:?}", off_wall(&ed));
    ed
}

/// S08 of the live run: the villa on a 3 m fall to the north, placed on its plot.
#[test]
fn the_sloped_villa_with_hip_roof_and_terrain_applies() {
    let ed = drafts_and_applies(json!({"typology": "house", "floors": 2, "floor_height": 3.0,
        "footprint": {"width": 10, "depth": 11},
        "roof": {"kind": "hip", "slope": 35, "overhang": 0.6},
        "stair": {"max_riser": 0.17},
        "origin": [14, 10],
        "site": {"plot": [[0, 0], [40, 0], [40, 30], [0, 30]],
                 "terrain": {"slope": 0.1, "downhill": [0, 1]}}}));
    assert!(count(&ed.model, "roof") >= 1);
    assert!(count(&ed.model, "stair") >= 1);
    assert!(count(&ed.model, "toposolid") >= 1);
}

/// S12 of the live run: seven storeys, shops below offices, U round a court, curtain wall.
#[test]
fn the_u_shaped_mixed_use_block_applies() {
    for width in [30, 36] {
        drafts_and_applies(json!({"typology": "mixed_use", "floors": 7,
            "footprint": {"width": width, "depth": 16},
            "design": {"massing": "u", "facade": "curtain"}}));
    }
}
