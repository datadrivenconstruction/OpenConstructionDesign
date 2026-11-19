//! H04, the 40,000-seat stadium, through the chat: the cantilever truss ring from the
//! steel_frame generator, then the bowl as levels, tier slabs, curved walls and stairs from
//! `create_elements`; Apply once, then every oracle of the case on the applied model.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_app::editor::Editor;
use e2e_ai_common::*;
use serde_json::{json, Value as J};

fn ring(r: f64, n: usize) -> J {
    json!((0..n).map(|i| {
        let t = std::f64::consts::TAU * i as f64 / n as f64;
        [r * 1.2 * t.cos(), r * t.sin()]
    }).collect::<Vec<_>>())
}

#[test]
fn h04_stadium_through_the_tools_meets_every_oracle() {
    let mut ed = Editor::new_in_memory().unwrap();
    let mut calls = vec![
        tool("help", json!({"topic": "stadium_ring"})),
        tool("load_tools", json!({"groups": ["scripts"]})),
        tool(
            "run_generator",
            json!({"id": "steel_frame", "variants": 1, "params": {"kind": "cantilever_ring", "span": 200,
                   "width": 160, "reach": 25, "backspan": 8, "height": 30, "bays": 40}}),
        ),
    ];
    for (i, z) in [0.0, 6.0, 12.0].into_iter().enumerate() {
        let name = format!("Tier {}", i + 1);
        calls.push(tool("create_level", json!({"title": name, "name": name, "elevation": z})));
    }
    let slabs: Vec<J> = (0..6)
        .map(|i| json!({"outer": ring(40.0 + 8.0 * i as f64, 48), "level": format!("Tier {}", i % 3 + 1)}))
        .collect();
    calls.push(tool("create_elements", json!({"title": "Tier slabs", "cat": "floor", "items": slabs})));
    // the bowl's facade: curved wall segments round the oval
    let walls: Vec<J> = (0..48)
        .map(|i| {
            let (a, b) = (ring(90.0, 48)[i].clone(), ring(90.0, 48)[(i + 1) % 48].clone());
            json!({"a": a, "b": b, "bulge": 0.02, "level": "Tier 1", "height": 12.0})
        })
        .collect();
    calls.push(tool("create_elements", json!({"title": "Bowl wall", "cat": "wall", "items": walls})));
    let stairs: Vec<J> = (0..24)
        .map(|i| {
            let p = ring(70.0, 24)[i].clone();
            json!({"origin": p, "angle": 0.0, "level": "Tier 1", "top_level": "Tier 2"})
        })
        .collect();
    calls.push(tool("create_elements", json!({"title": "Vomitory stairs", "cat": "stair", "items": stairs})));
    let n = calls.len();
    let (mut s, provider) = run(&mut ed, calls);
    let res = tool_results(&provider);
    assert_eq!(res.len(), n, "{res:?}");
    assert!(res.iter().all(|r| !r.1), "{res:?}");
    meets_every_oracle(
        "H04_stadium_roof.toml",
        &mut s,
        &mut ed,
        "Stadium: three tiers, curved bowl wall, vomitory stairs and a cantilever truss ring roof of 25 m reach with 8 m back-span.",
    );
}
