// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Edge-case projects: what a model must survive whole. Each builder states the behaviour it
//! relies on; the scenario tests check the result like any other project.

use ddc_model::{
    CompHost, CompSpec, GridSpec, Model, PVal, ParamSet, ParamSpec, ParamType, RoomSpec, SlabSpec,
    WallSpec,
};

use super::{door, lib_type, rect, ring, slab_type, wall, wall_type, window};

/// A fresh model with nothing added.
pub fn empty(_: &mut Model) {}

/// One level, 12 x 12 cells of 20 m (240 m square), a room per cell, one slab over all.
pub fn huge_level(m: &mut Model) {
    huge_level_n(m, 12);
}

/// `n` x `n` cells of 20 m on one level: walls split at every node, a door in every vertical
/// wall, a room per cell, one slab under everything.
pub fn huge_level_n(m: &mut Model, n: usize) {
    const CELL: f64 = 20.0;
    let lv = m.add_level("Hall level", 0.0).unwrap();
    let t = wall_type(m, "Hall wall 200", &[("concrete", 0.2)]);
    let st = slab_type(m, "Hall slab 300", &[("concrete", 0.3)]);
    let c = |k: usize| k as f64 * CELL;
    let mut ws = vec![];
    for j in 0..=n {
        for i in 0..n {
            ws.push(wall(lv, t, (c(i), c(j)), (c(i + 1), c(j)), 4.0));
            ws.push(wall(lv, t, (c(j), c(i)), (c(j), c(i + 1)), 4.0));
        }
    }
    let ids = m.add_walls(&ws).unwrap();
    // odd entries are the vertical walls; inner ones get a door in the middle
    for (k, &w) in ids.iter().enumerate() {
        let j = k / (2 * n);
        if k % 2 == 1 && j > 0 && j < n {
            m.add_opening(w, &door(CELL / 2.0 - 0.5, 1.0)).unwrap();
        }
    }
    let rooms: Vec<RoomSpec> = (0..n * n)
        .map(|k| RoomSpec {
            level: lv,
            name: format!("Cell {}-{}", k / n + 1, k % n + 1),
            number: format!("{:04}", k + 1),
            point: (c(k % n) + CELL / 2.0, c(k / n) + CELL / 2.0),
            height: 4.0,
        })
        .collect();
    m.add_rooms(&rooms).unwrap();
    m.add_slab(&SlabSpec {
        level: lv,
        slab_type: st,
        offset: 0.0,
        outer: rect(0.0, 0.0, c(n), c(n)),
        holes: vec![],
    })
    .unwrap();
}

/// Wall cases: a zero-length wall is refused and leaves the model as it was; overlapping
/// corners and collinear overlaps are accepted with warnings; crossing walls, a 10 mm wall and
/// an arc wall are ordinary walls.
pub fn walls(m: &mut Model) {
    let lv = m.add_level("L0", 0.0).unwrap();
    let t = wall_type(m, "Edge 200", &[("brick", 0.1), ("insulation", 0.1)]);
    let thin = wall_type(m, "Edge 100", &[("gypsum", 0.1)]);

    let zero = wall(lv, t, (5.0, 5.0), (5.0, 5.0), 3.0);
    let before = super::dump(m, super::Keep::All);
    assert!(m.add_wall(&zero).is_err(), "a zero-length wall is refused");
    assert!(m.add_walls(&[zero]).is_err(), "a zero-length wall is refused in a batch");
    let after = super::dump(m, super::Keep::All);
    assert!(
        super::diff(&after, &before).is_empty(),
        "a refused wall changes nothing"
    );

    let mut corner = wall(lv, t, (0.0, 0.0), (1.0, 0.0), 3.0);
    corner.offset0 = Some(0.0);
    let mut back = wall(lv, t, (1.0, 0.0), (0.0, 0.1), 3.0);
    back.offset0 = Some(0.0);
    m.add_walls(&[corner, back]).unwrap();
    m.add_walls(&[
        wall(lv, t, (0.0, 10.0), (6.0, 10.0), 3.0),
        wall(lv, t, (3.0, 10.0), (9.0, 10.0), 3.0),
    ])
    .unwrap();
    // an X crossing, a T, an L
    m.add_walls(&[
        wall(lv, thin, (20.0, 0.0), (20.0, 8.0), 3.0),
        wall(lv, thin, (16.0, 4.0), (24.0, 4.0), 3.0),
        wall(lv, thin, (16.0, 0.0), (16.0, 4.0), 3.0),
        wall(lv, thin, (24.0, 8.0), (20.0, 8.0), 3.0),
    ])
    .unwrap();
    m.add_wall(&wall(lv, thin, (30.0, 0.0), (30.01, 0.0), 3.0)).unwrap();
    m.add_arc_wall(&wall(lv, t, (40.0, 0.0), (48.0, 0.0), 3.0), 0.4)
        .unwrap();
    let w = m.add_wall(&wall(lv, t, (40.0, 10.0), (48.0, 10.0), 3.0)).unwrap();
    m.add_opening(w, &window(0.5, 2.0, 0.9, 1.5)).unwrap();
    m.add_opening(w, &door(5.0, 1.0)).unwrap();
    assert!(!m.wall_warnings().unwrap().is_empty(), "overlaps are warned about");
}

/// A walled room with a door, a window, two wall-hosted and one floor-hosted component; the
/// wall with the openings and the hosted components, then the slab with its bed, are deleted.
pub fn deleted_host(m: &mut Model) {
    let lv = m.add_level("L1", 0.0).unwrap();
    let t = wall_type(m, "Host 250", &[("masonry", 0.25)]);
    let st = slab_type(m, "Host slab 200", &[("concrete", 0.2)]);
    let ws = m.add_walls(&ring(lv, t, &rect(0.0, 0.0, 6.0, 5.0), 3.0)).unwrap();
    m.add_opening(ws[0], &door(1.0, 0.9)).unwrap();
    m.add_opening(ws[0], &window(2.5, 1.5, 0.9, 1.2)).unwrap();
    m.add_opening(ws[1], &window(3.0, 1.2, 0.9, 1.2)).unwrap();
    let slab = m
        .add_slab(&SlabSpec {
            level: lv,
            slab_type: st,
            offset: 0.0,
            outer: rect(0.0, 0.0, 6.0, 5.0),
            holes: vec![],
        })
        .unwrap();
    m.add_room(&RoomSpec {
        level: lv,
        name: "Host room".into(),
        number: "1".into(),
        point: (3.0, 2.5),
        height: 2.8,
    })
    .unwrap();
    let basin = lib_type(m, "Washbasin", "500");
    let radiator = lib_type(m, "Radiator", "1000x500");
    let bed = lib_type(m, "Bed", "Single 900x2000");
    let table = lib_type(m, "Table", "1200x800");
    // clear of the openings: the basin past the window, the radiator before it
    let mut a = CompSpec::at(lv, basin, 5.0, 0.1);
    a.host = CompHost::Wall(ws[0]);
    let mut r = CompSpec::at(lv, radiator, 0.9, 0.1);
    r.host = CompHost::Wall(ws[1]);
    let mut b = CompSpec::at(lv, bed, 2.0, 3.0);
    b.host = CompHost::Floor(slab);
    m.add_components(&[a, r, b, CompSpec::at(lv, table, 4.0, 3.0)])
        .unwrap();
    m.delete_wall(ws[0]).unwrap();
    m.delete_slab(slab).unwrap();
}

/// Survey-size coordinates (500 km east, 6000 km north), a level 45 m below and one 320 m above
/// the base, a 100 m wall, a type with a 1 mm layer, a far grid line, a tiny room.
pub fn extreme(m: &mut Model) {
    const E: f64 = 500_000.0;
    const N: f64 = 6_000_000.0;
    let deep = m.add_level("Deep -45", -45.0).unwrap();
    let base = m.add_level("Base", 0.0).unwrap();
    let high = m.add_level("High 320", 320.0).unwrap();
    let t = wall_type(m, "Far 300", &[("concrete", 0.3)]);
    let foil = wall_type(m, "Foil 1 mm", &[("membrane", 0.001), ("gypsum", 0.0125)]);
    let st = slab_type(m, "Far slab 250", &[("concrete", 0.25)]);
    for (lv, h) in [(deep, 4.0), (base, 3.0), (high, 100.0)] {
        let pts = rect(E, N, E + 10.0, N + 8.0);
        let ws = m.add_walls(&ring(lv, t, &pts, h)).unwrap();
        m.add_opening(ws[0], &door(1.0, 0.9)).unwrap();
        m.add_opening(ws[2], &window(2.0, 1.5, 0.9, 1.2)).unwrap();
        m.add_wall(&wall(lv, foil, (E + 5.0, N), (E + 5.0, N + 8.0), h)).unwrap();
        m.add_slab(&SlabSpec {
            level: lv,
            slab_type: st,
            offset: 0.0,
            outer: pts.clone(),
            holes: vec![],
        })
        .unwrap();
        m.add_room(&RoomSpec {
            level: lv,
            name: "Far room".into(),
            number: format!("{lv}"),
            point: (E + 2.5, N + 4.0),
            height: h.min(3.0),
        })
        .unwrap();
    }
    // a 0.6 x 0.6 m shaft room at the origin
    let small = m.add_walls(&ring(base, foil, &rect(0.0, 0.0, 0.6, 0.6), 3.0)).unwrap();
    assert_eq!(small.len(), 4);
    m.add_room(&RoomSpec {
        level: base,
        name: "Tiny".into(),
        number: "T".into(),
        point: (0.3, 0.3),
        height: 3.0,
    })
    .unwrap();
    m.add_grid(&GridSpec::line("Far", (E - 50.0, N - 50.0), (E + 60.0, N - 50.0)))
        .unwrap();
}

/// Metric and imperial: imperial levels, walls and library types beside metric ones, and
/// project parameters of every type (text, integer, number in several units, length, area,
/// volume, yes/no) on instances and on types.
pub fn units(m: &mut Model) {
    const FT: f64 = 0.3048;
    const IN: f64 = 0.0254;
    let metric = m.add_level("Metric 0", 0.0).unwrap();
    let imperial = m.add_level("Imperial 10'", 10.0 * FT).unwrap();
    let mt = wall_type(m, "Metric 175", &[("plaster", 0.015), ("masonry", 0.16)]);
    let it = wall_type(m, "Imperial 8in", &[("gypsum", 0.5 * IN), ("wood", 7.5 * IN)]);
    let mut ids = m
        .add_walls(&ring(metric, mt, &rect(0.0, 0.0, 7.255, 4.825), 2.755))
        .unwrap();
    let pts = rect(0.0, 0.0, 24.0 * FT, 16.0 * FT);
    let spec: Vec<WallSpec> = ring(imperial, it, &pts, 8.0 * FT);
    ids.extend(m.add_walls(&spec).unwrap());
    m.add_opening(ids[0], &door(1.0, 0.9)).unwrap();
    m.add_opening(ids[4], &door(3.0 * FT, 3.0 * FT)).unwrap();
    m.add_opening(ids[6], &window(4.0 * FT, 4.0 * FT, 3.0 * FT, 4.0 * FT)).unwrap();
    for (lv, name, point) in [
        (metric, "Metric room", (3.0, 2.0)),
        (imperial, "Imperial room", (12.0 * FT, 8.0 * FT)),
    ] {
        m.add_room(&RoomSpec {
            level: lv,
            name: name.into(),
            number: name[..1].into(),
            point,
            height: 2.5,
        })
        .unwrap();
    }
    let desk = lib_type(m, "Desk", "60x30in");
    let table = lib_type(m, "Table", "1600x800");
    m.add_components(&[
        CompSpec::at(imperial, desk, 6.0 * FT, 4.0 * FT),
        CompSpec::at(metric, table, 3.5, 2.4),
    ])
    .unwrap();

    let p = |name: &str, ptype, per_type, unit: &str| ParamSpec {
        name: name.into(),
        ptype,
        per_type,
        unit: unit.into(),
        group: "Units".into(),
        guid: None,
        categories: vec!["wall".into()],
    };
    let defs = [
        (p("Note", ParamType::Text, false, ""), PVal::Text("as built".into())),
        (p("Count", ParamType::Int, false, ""), PVal::Int(42)),
        (p("Mass", ParamType::Number, false, "kg"), PVal::Num(1234.5)),
        (p("Share", ParamType::Number, false, "%"), PVal::Num(12.5)),
        (p("Pieces", ParamType::Number, false, "pcs"), PVal::Num(7.0)),
        (p("Gap", ParamType::Length, false, ""), PVal::Num(0.0125)),
        (p("Face", ParamType::Area, false, ""), PVal::Num(18.75)),
        (p("Fill", ParamType::Volume, false, ""), PVal::Num(2.5)),
        (p("Load bearing", ParamType::YesNo, false, ""), PVal::Bool(true)),
        (p("Type depth", ParamType::Length, true, ""), PVal::Num(8.0 * IN)),
        (p("Type code", ParamType::Int, true, ""), PVal::Int(-3)),
        (p("Type rated", ParamType::YesNo, true, ""), PVal::Bool(false)),
    ];
    let mut sets = vec![];
    for (spec, value) in defs {
        let per_type = spec.per_type;
        let id = m.add_param(&spec).unwrap();
        if per_type {
            for ty in [mt, it] {
                sets.push(ParamSet {
                    param: id,
                    kind: "wall_type".into(),
                    id: ty,
                    value: Some(value.clone()),
                });
            }
        } else {
            for &w in &ids {
                sets.push(ParamSet {
                    param: id,
                    kind: "wall".into(),
                    id: w,
                    value: Some(value.clone()),
                });
            }
        }
    }
    m.set_param_values(&sets).unwrap();
}
