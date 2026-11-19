// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Office: 24 x 14 m, three levels of offices along a corridor, grid A-E / 1-3, four layered
//! wall types, doors and windows, named and numbered rooms, two stairs with railings, floor
//! slabs (one with a stair hole) and a roof, ducts and pipes with bends and tees, a desk and a
//! chair per office, project parameters, tags, a dimension, two sheets with plans, a section
//! and schedules.

use ddc_model::{
    CompHost, CompSpec, DimKind, DimRef, DimSpec, GridSpec, MepDomain, MepSection, MepShape,
    Model, PVal, ParamSet, ParamSpec, ParamType, PipeMaterial, RailingHost, RailingSpec, RoofKind,
    RoofSpec, RoomSpec, RunSpec, SchedulePlace, SectionSpec, SheetDef, SlabSpec, StairSpec,
    SystemKind, TagCat, ViewKind, ViewRef, ViewportDef, DEFAULT_CUT,
};

use super::{door, ensure_material, hole, lib_type, rect, ring, slab_type, wall, wall_type, window};

const W: f64 = 24.0;
const D: f64 = 14.0;
const STOREY: f64 = 3.6;
/// Corridor between y = 6 and y = 8.
const CORR: (f64, f64) = (6.0, 8.0);

pub fn build(m: &mut Model) {
    let levels: Vec<i64> = ["Level 1", "Level 2", "Level 3"]
        .iter()
        .enumerate()
        .map(|(k, n)| m.add_level(n, k as f64 * STOREY).unwrap())
        .collect();
    for (k, name) in ["A", "B", "C", "D", "E"].iter().enumerate() {
        let x = 6.0 * k as f64;
        m.add_grid(&GridSpec::line(name, (x, -2.0), (x, D + 2.0))).unwrap();
    }
    for (k, name) in ["1", "2", "3"].iter().enumerate() {
        let y = 7.0 * k as f64;
        m.add_grid(&GridSpec::line(name, (-2.0, y), (W + 2.0, y))).unwrap();
    }

    let ext = wall_type(
        m,
        "Exterior 365",
        &[("render", 0.015), ("masonry", 0.3), ("insulation", 0.05)],
    );
    let int = wall_type(
        m,
        "Interior 125",
        &[("gypsum", 0.0125), ("insulation", 0.1), ("gypsum", 0.0125)],
    );
    let part = wall_type(m, "Partition 100", &[("gypsum", 0.1)]);
    let core = wall_type(m, "Core 250", &[("concrete", 0.25)]);

    let floor = slab_type(
        m,
        "Floor 250",
        &[("wood", 0.025), ("membrane", 0.005), ("concrete", 0.22)],
    );
    let ground = slab_type(m, "Slab 200", &[("concrete", 0.2)]);
    let roof_t = slab_type(
        m,
        "Roof 300",
        &[("membrane", 0.01), ("insulation", 0.2), ("concrete", 0.09)],
    );

    for (k, &lv) in levels.iter().enumerate() {
        storey(m, lv, k, (ext, int, part, core));
    }

    // floors: the ground slab, upper floors with the stair hole
    m.add_slab(&SlabSpec {
        level: levels[0],
        slab_type: ground,
        offset: 0.0,
        outer: rect(0.0, 0.0, W, D),
        holes: vec![],
    })
    .unwrap();
    for &lv in &levels[1..] {
        m.add_slab(&SlabSpec {
            level: lv,
            slab_type: floor,
            offset: 0.0,
            outer: rect(0.0, 0.0, W, D),
            holes: vec![hole(0.4, CORR.0 + 0.1, 5.6, CORR.1 - 0.1)],
        })
        .unwrap();
    }
    m.add_roof(&RoofSpec {
        level: levels[2],
        slab_type: roof_t,
        offset: STOREY,
        overhang: 0.5,
        fascia: 0.2,
        kind: RoofKind::Footprint {
            ring: rect(0.0, 0.0, W, D),
            slopes: vec![Some(15.0), None, Some(15.0), None],
            mansard: None,
        },
    })
    .unwrap();

    // stairs between the levels, each with its two railings
    ensure_material(m, "Steel");
    for k in 0..2 {
        let st = m
            .add_stair(&StairSpec::straight(levels[k], levels[k + 1], (0.6, 6.4), 0.0))
            .unwrap();
        m.add_stair_railings(
            st,
            &RailingSpec::new(levels[k], RailingHost::Stair { stair: st, side: 0 }),
        )
        .unwrap();
    }

    mep(m, &levels);
    furniture(m, levels[0]);
    params(m, ext);
    annotations(m, &levels);
}

/// Ring, corridor walls, partitions, doors, windows and rooms of one storey.
fn storey(m: &mut Model, lv: i64, k: usize, (ext, int, part, core): (i64, i64, i64, i64)) {
    let ring_ids = m.add_walls(&ring(lv, ext, &rect(0.0, 0.0, W, D), STOREY)).unwrap();
    let corr: Vec<i64> = [CORR.0, CORR.1]
        .iter()
        .map(|&y| m.add_wall(&wall(lv, int, (0.0, y), (W, y), STOREY)).unwrap())
        .collect();
    let mut parts = vec![];
    for x in [6.0, 12.0, 18.0] {
        parts.push(wall(lv, part, (x, 0.0), (x, CORR.0), STOREY));
        parts.push(wall(lv, part, (x, CORR.1), (x, D), STOREY));
    }
    m.add_walls(&parts).unwrap();
    // a concrete core wall closes the stair end of the corridor on the upper storeys
    if k > 0 {
        m.add_wall(&wall(lv, core, (6.0, CORR.0), (6.0, CORR.1), STOREY)).unwrap();
    }
    // south (ring 0) and north (ring 2) facades: two windows per 6 m bay
    for &w in [ring_ids[0], ring_ids[2]].iter() {
        for bay in 0..4 {
            let x = 6.0 * bay as f64;
            m.add_opening(w, &window(x + 1.75, 1.2, 0.9, 1.5)).unwrap();
            m.add_opening(w, &window(x + 3.75, 1.2, 0.9, 1.5)).unwrap();
        }
    }
    // east facade (ring 1, from (W,0) to (W,D)): entrance below, a window above
    if k == 0 {
        m.add_opening(ring_ids[1], &door(6.5, 1.0)).unwrap();
    } else {
        m.add_opening(ring_ids[1], &window(6.4, 1.2, 0.9, 1.5)).unwrap();
    }
    for &w in &corr {
        for bay in 0..4 {
            m.add_opening(w, &door(6.0 * bay as f64 + 2.5, 0.9)).unwrap();
        }
    }
    let base = 100 * (k as i64 + 1);
    let mut rooms = vec![RoomSpec {
        level: lv,
        name: "Corridor".into(),
        number: format!("{base}"),
        point: (12.0, 7.0),
        height: STOREY,
    }];
    for bay in 0..4 {
        let x = 6.0 * bay as f64 + 3.0;
        for (j, y) in [3.0, 11.0].iter().enumerate() {
            rooms.push(RoomSpec {
                level: lv,
                name: if bay == 0 && j == 0 { "Meeting".into() } else { "Office".into() },
                number: format!("{}", base + 1 + 2 * bay as i64 + j as i64),
                point: (x, *y),
                height: STOREY - 0.3,
            });
        }
    }
    m.add_rooms(&rooms).unwrap();
}

/// Supply air duct with a bend and a tee branch, cold water pipe with a tee and a riser.
fn mep(m: &mut Model, levels: &[i64]) {
    let sa = m.add_mep_system("SA-1", SystemKind::SupplyAir).unwrap();
    let cw = m.add_mep_system("CW-1", SystemKind::ColdWater).unwrap();
    for (k, &lv) in levels.iter().enumerate().take(2) {
        let z = k as f64 * STOREY + 3.0;
        let mut duct = MepSection::new(MepDomain::Duct, MepShape::Rect { w: 0.5, h: 0.25 });
        duct.insulation = 0.03;
        m.add_mep_run(&RunSpec::new(
            sa,
            lv,
            duct,
            vec![(1.0, 7.0, z), (23.0, 7.0, z), (23.0, 10.0, z)],
        ))
        .unwrap();
        let branch = MepSection::new(MepDomain::Duct, MepShape::Round { d: 0.2 });
        m.add_mep_run(&RunSpec::new(sa, lv, branch, vec![(9.0, 7.0, z), (9.0, 4.0, z)]))
            .unwrap();
    }
    let main = RunSpec::pipe(
        cw,
        levels[0],
        PipeMaterial::Copper,
        25,
        vec![(0.5, 8.5, 0.5), (23.5, 8.5, 0.5)],
    )
    .unwrap();
    m.add_mep_run(&main).unwrap();
    for spec in [
        vec![(12.0, 8.5, 0.5), (12.0, 10.0, 0.5)],
        vec![(23.5, 8.5, 0.5), (23.5, 8.5, STOREY + 0.5)],
    ] {
        let p = RunSpec::pipe(cw, levels[0], PipeMaterial::Copper, 15, spec).unwrap();
        m.add_mep_run(&p).unwrap();
    }
}

/// A desk and a chair in every ground floor office, a washbasin on the corridor wall.
fn furniture(m: &mut Model, lv: i64) {
    let desk = lib_type(m, "Desk", "1400x700");
    let chair = lib_type(m, "Chair", "450x500");
    let basin = lib_type(m, "Washbasin", "500");
    let mut specs = vec![];
    for bay in 0..4 {
        let x = 6.0 * bay as f64 + 3.0;
        for y in [2.5, 11.5] {
            specs.push(CompSpec::at(lv, desk, x, y));
            let mut c = CompSpec::at(lv, chair, x, y + if y < 7.0 { 0.7 } else { -0.7 });
            c.rotation = if y < 7.0 { std::f64::consts::PI } else { 0.0 };
            specs.push(c);
        }
    }
    m.add_components(&specs).unwrap();
    let corr = super::first_id(
        m,
        &format!("SELECT id FROM wall WHERE level={lv} AND ay=6.0 AND by=6.0 ORDER BY id"),
    )
    .expect("corridor wall");
    // clear of the doors at 20.5..21.4 along the wall
    let mut b = CompSpec::at(lv, basin, 22.8, 0.1);
    b.host = CompHost::Wall(corr);
    m.add_component(&b).unwrap();
}

/// A type parameter on the exterior wall type and an instance cost on every component.
fn params(m: &mut Model, ext: i64) {
    let spec = |name: &str, ptype, per_type, unit: &str, cat: &str| ParamSpec {
        name: name.into(),
        ptype,
        per_type,
        unit: unit.into(),
        group: "Office".into(),
        guid: None,
        categories: vec![cat.into()],
    };
    let fire = m
        .add_param(&spec("Fire rating", ParamType::Text, true, "", "wall"))
        .unwrap();
    let cost = m
        .add_param(&spec("Cost", ParamType::Number, false, "EUR", "comp"))
        .unwrap();
    let mut sets = vec![ParamSet {
        param: fire,
        kind: "wall_type".into(),
        id: ext,
        value: Some(PVal::Text("REI 90".into())),
    }];
    let comps: Vec<i64> = {
        let mut st = m.sql().prepare("SELECT id FROM comp ORDER BY id").unwrap();
        st.query_map([], |r| r.get(0)).unwrap().map(|x| x.unwrap()).collect()
    };
    for (k, id) in comps.into_iter().enumerate() {
        sets.push(ParamSet {
            param: cost,
            kind: "comp".into(),
            id,
            value: Some(PVal::Num(150.0 + 25.0 * k as f64)),
        });
    }
    m.set_param_values(&sets).unwrap();
}

/// Tags and a dimension on the ground floor, a section, two sheets with views and schedules.
fn annotations(m: &mut Model, levels: &[i64]) {
    let l1 = levels[0];
    m.tag_all(l1, &[TagCat::Door, TagCat::Window, TagCat::Room], false)
        .unwrap();
    let south = super::first_id(
        m,
        &format!("SELECT id FROM wall WHERE level={l1} ORDER BY id"),
    )
    .expect("south wall");
    m.add_dim(&DimSpec {
        level: l1,
        kind: DimKind::Linear,
        refs: vec![
            DimRef::AxisEnd { wall: south, end: 0 },
            DimRef::AxisEnd { wall: south, end: 1 },
        ],
        dir: (0.0, 0.0),
        side: -1.0,
        offset: 1.5,
        tier: -1,
        auto: false,
    })
    .unwrap();
    let section = m
        .add_view_def(&SectionSpec {
            kind: ViewKind::Section,
            name: "Section A-A".into(),
            a: (-1.0, 3.0),
            b: (W + 1.0, 3.0),
            depth: 8.0,
            far_clip: false,
        })
        .unwrap();

    let plans = m.add_sheet(&SheetDef::new("A-101", "Ground floor plan")).unwrap();
    m.add_viewport(&ViewportDef {
        sheet: plans,
        view: ViewRef::Plan {
            level: l1,
            cut: DEFAULT_CUT,
        },
        scale: 100.0,
        center: (150.0, 160.0),
        crop: None,
        title: "Level 1".into(),
    })
    .unwrap();
    m.add_schedule_place(&SchedulePlace {
        sheet: plans,
        schedule: "rooms".into(),
        pos: (300.0, 280.0),
    })
    .unwrap();

    let other = m
        .add_sheet(&SheetDef::new("A-201", "Upper floor and section"))
        .unwrap();
    m.add_viewport(&ViewportDef {
        sheet: other,
        view: ViewRef::Plan {
            level: levels[1],
            cut: DEFAULT_CUT,
        },
        scale: 100.0,
        center: (150.0, 200.0),
        crop: None,
        title: "Level 2".into(),
    })
    .unwrap();
    m.add_viewport(&ViewportDef {
        sheet: other,
        view: ViewRef::View(section),
        scale: 100.0,
        center: (150.0, 70.0),
        crop: None,
        title: "Section A-A".into(),
    })
    .unwrap();
    for (key, y) in [("walls", 280.0), ("doors", 150.0)] {
        m.add_schedule_place(&SchedulePlace {
            sheet: other,
            schedule: key.into(),
            pos: (300.0, y),
        })
        .unwrap();
    }
}
