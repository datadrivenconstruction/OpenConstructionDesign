// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use ddc_model::*;
fn run(f: impl Fn(&mut WallSpec, i64)) -> (i64, i64, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let l2 = m.add_level("L2", 3.5).unwrap();
    let t = m.add_wall_type("ext", &[LayerSpec { material: "brick".into(), thickness: 0.3 }, LayerSpec { material: "ins".into(), thickness: 0.1 }]).unwrap();
    let w = m.add_wall(&WallSpec { a: (0.0, 0.0), b: (4.0, 0.0), level: l1, base_offset: 0.0, height: 3.0, wall_type: t, offset0: None }).unwrap();
    let n = |m: &Model| m.sql().query_row("SELECT count(*) FROM wall_qty", [], |r| r.get::<_, i64>(0)).unwrap();
    let before = n(&m);
    let mut spec = WallSpec { a: (0.0, 0.0), b: (4.0, 0.0), level: l1, base_offset: 0.0, height: 3.0, wall_type: t, offset0: None };
    f(&mut spec, l2);
    m.apply_edits(&[Edit::Wall { id: w, spec, bulge: 0.0 }]).unwrap();
    let mid = n(&m);
    m.undo().unwrap();
    (before, mid, n(&m))
}
#[test]
fn undo_of_a_wall_level_change_keeps_its_quantities() {
    let h = run(|s, _| s.height = 3.5);
    let l = run(|s, l2| s.level = l2);
    assert_eq!((h, l), ((2, 2, 2), (2, 2, 2)));
}
