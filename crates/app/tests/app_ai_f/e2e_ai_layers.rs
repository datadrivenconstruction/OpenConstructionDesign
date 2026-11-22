//! Layered build-ups through Apply: the generated walls, floors and roofs carry the layers the
//! project's rules chose, a colder climate's pack thickens the exterior wall (the rooms lose
//! net area, the walls still join), and an IFC round trip keeps the layers and the physics of
//! their materials.

use crate::{e2e_ai_common, e2e_common};

use std::collections::BTreeSet;

use ddc_app::editor::Editor;
use ddc_io::ifc::{export_ifc, import_ifc};
use ddc_model::Model;
use e2e_ai_common::*;
use e2e_common::assert_clean;
use serde_json::{json, Value as J};

fn applied(spec: J, pack: &str) -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    let (mut s, provider) = run(&mut ed, vec![tool("generate_project", json!({"spec": spec, "pack": pack}))]);
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{pack}: {results:?}");
    apply_and_check(&mut s, &mut ed);
    ed
}

fn block() -> J {
    json!({"typology": "apartment_section", "floors": 2})
}

type Layers = Vec<(String, i64)>;

/// The layers (material, mm) of every type the elements of `table` use.
fn used_layers(m: &Model, table: &str) -> BTreeSet<(String, Layers)> {
    // walls take a wall type; slabs and roofs a slab type
    let (ty, col) = if table == "wall" { ("wall_type", "wtype") } else { ("slab_type", "stype") };
    let key = col;
    let sql = format!("SELECT DISTINCT t.id, t.name FROM {table} e JOIN {ty} t ON t.id = e.{col}");
    let mut st = m.sql().prepare(&sql).unwrap();
    let types: Vec<(i64, String)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect();
    types
        .into_iter()
        .map(|(id, name)| {
            let sql = format!("SELECT material, thickness FROM {ty}_layer WHERE {key}=?1 ORDER BY pos");
            let mut st = m.sql().prepare(&sql).unwrap();
            let layers: Layers = st
                .query_map([id], |r| Ok((r.get::<_, String>(0)?, (r.get::<_, f64>(1)? * 1000.0).round() as i64)))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            (name, layers)
        })
        .collect()
}

fn total(l: &Layers) -> i64 {
    l.iter().map(|x| x.1).sum()
}

fn room_area(m: &Model) -> f64 {
    m.level_totals().unwrap().iter().map(|t| t.totals.room_area).sum()
}

#[test]
fn generated_elements_carry_their_build_ups_and_a_colder_pack_thickens_the_wall() {
    let (de, uk) = (applied(block(), "de"), applied(block(), "uk"));
    for (pack, ed) in [("de", &de), ("uk", &uk)] {
        assert_clean(&ed.model, pack);
        for table in ["wall", "slab", "roof"] {
            let types = used_layers(&ed.model, table);
            assert!(!types.is_empty(), "{pack}: no {table}");
            for (name, layers) in &types {
                assert!(layers.len() >= 3, "{pack}: {table} type {name} is single-layer: {layers:?}");
            }
        }
        let n: i64 = ed.model.sql().query_row("SELECT count(*) FROM room", [], |r| r.get(0)).unwrap();
        assert!(n > 0 && room_area(&ed.model) > 0.0, "{pack}: {n} rooms");
    }
    let ext = |ed: &Editor| -> i64 {
        let walls = used_layers(&ed.model, "wall");
        let e: BTreeSet<i64> = walls.iter().filter(|w| w.0.starts_with("Gen Exterior")).map(|w| total(&w.1)).collect();
        assert_eq!(e.len(), 1, "{walls:?}");
        e.into_iter().next().unwrap()
    };
    // the pack's U-value (0.28 against 0.26 W/m2K) sizes the insulation
    assert_eq!((ext(&de), ext(&uk)), (300, 320));
    // the exterior walls listed inside first hold the same layers turned round
    let walls = used_layers(&uk.model, "wall");
    let fwd: Vec<&Layers> = walls.iter().filter(|w| w.0.starts_with("Gen Exterior") && !w.0.contains("inside first")).map(|w| &w.1).collect();
    for (_, rev) in walls.iter().filter(|w| w.0.contains("inside first")) {
        let mut back = rev.clone();
        back.reverse();
        assert!(fwd.contains(&&back), "{rev:?}");
    }
    // the thicker wall comes off the rooms: the same plan nets less floor area
    let (a, b) = (room_area(&de.model), room_area(&uk.model));
    assert!(b < a && b > a * 0.9, "room area {a} -> {b}");
    // the insulation's physics reaches the material table
    let mw = uk.model.material("Mineral Wool Facade Insulation").unwrap();
    assert!(mw.id != 0 && mw.spec.conductivity == Some(0.035) && mw.spec.density == Some(140.0), "{mw:?}");
}

#[test]
fn an_ifc_round_trip_keeps_the_layers() {
    let ed = applied(block(), "uk");
    let text = export_ifc(&ed.model).unwrap();
    assert!(text.contains("IFCMATERIALLAYERSET("), "no layer set in the export");
    let mut back = Model::open(":memory:").unwrap();
    import_ifc(&mut back, text.as_bytes()).unwrap();
    for table in ["wall", "slab", "roof"] {
        let lists = |m: &Model| -> BTreeSet<Layers> { used_layers(m, table).into_iter().map(|t| t.1).collect() };
        let (was, now) = (lists(&ed.model), lists(&back));
        assert!(!was.is_empty(), "{table}");
        assert_eq!(was, now, "{table}: layers lost on the round trip");
    }
    for name in ["Mineral Wool Facade Insulation", "Reinforced Concrete"] {
        let (a, b) = (ed.model.material(name).unwrap(), back.material(name).unwrap());
        assert!(b.id != 0, "{name} not imported");
        assert_eq!((a.spec.conductivity, a.spec.density), (b.spec.conductivity, b.spec.density), "{name}");
    }
}
