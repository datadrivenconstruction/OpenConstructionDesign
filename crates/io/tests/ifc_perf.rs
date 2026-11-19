// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Time of export, post-pass, validation and import on 10k walls (printed; run in release).

use std::time::Instant;

use ddc_io::ifc::{self, IfcOptions, IfcSchema};
use ddc_model::{LayerSpec, Model, WallSpec};

#[test]
fn ten_thousand_walls_timing() {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let layer = LayerSpec {
        material: "Brick".into(),
        thickness: 0.2,
    };
    let wt = m.add_wall_type("W", &[layer]).unwrap();
    let specs: Vec<WallSpec> = (0..10_000)
        .map(|k| {
            let (x, y) = ((k % 100) as f64 * 2.0, (k / 100) as f64 * 2.0);
            WallSpec {
                a: (x, y),
                b: (x + 1.5, y),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: Some(0.0),
            }
        })
        .collect();
    m.add_walls(&specs).unwrap();
    let mut line = String::from("IFC_PERF 10k walls:");
    let t = Instant::now();
    let raw = ifc::export_ifc(&m).unwrap();
    line += &format!(" export {:?}", t.elapsed());
    for (name, schema) in [("ifc4", IfcSchema::Ifc4), ("ifc2x3", IfcSchema::Ifc2x3)] {
        let opts = IfcOptions {
            schema,
            timestamp: Some("2026-01-01T00:00:00".into()),
            ..Default::default()
        };
        let t = Instant::now();
        let (text, _) = ifc::export_ifc_with(&m, &opts).unwrap();
        line += &format!(", {name} {:?}", t.elapsed());
        let t = Instant::now();
        let rep = ifc::validate_ifc(text.as_bytes());
        line += &format!(", validate {:?}", t.elapsed());
        assert!(rep.errors.is_empty(), "{name}: {:?}", rep.errors.first());
    }
    let mut back = Model::open(":memory:").unwrap();
    let t = Instant::now();
    let rep = ifc::import_ifc(&mut back, raw.as_bytes()).unwrap();
    line += &format!(", import {:?}, {} bytes", t.elapsed(), raw.len());
    eprintln!("{line}");
    assert_eq!(rep.walls, 10_000);
}
