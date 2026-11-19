// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Residential: a 12 x 9 m house on two levels with Cyrillic names and values throughout and
//! names at the length limits; a curtain wall in the south facade; library families with an
//! added type, instance values and wall, floor and free hosting; phases (existing walls, one
//! demolished partition, a phase of its own); project parameters; project info; a sheet.
//! Cyrillic is written as escapes (no Cyrillic in .rs files).

use ddc_model::{
    CompHost, CompSpec, CurtainSpec, GridLines, Model, MullionSection, PVal, ParamSet, ParamSpec,
    ParamType, ParamValue, ProjectInfo, RoofKind, RoofSpec, RoomSpec, SchedulePlace, SheetDef,
    SlabSpec, ViewRef, ViewportDef, DEFAULT_CUT,
};

use super::{door, lib_type, long_text, rect, slab_type, wall, wall_type, window};

/// "Floor 1", "Attic"
const LEVELS: [&str; 2] = [
    "\u{42d}\u{442}\u{430}\u{436} 1",
    "\u{41c}\u{430}\u{43d}\u{441}\u{430}\u{440}\u{434}\u{430}",
];
/// "Plaster", "Aerated concrete", "Insulation", "Brick"
const PLASTER: &str = "\u{428}\u{442}\u{443}\u{43a}\u{430}\u{442}\u{443}\u{440}\u{43a}\u{430}";
const AERATED: &str = "\u{413}\u{430}\u{437}\u{43e}\u{431}\u{435}\u{442}\u{43e}\u{43d}";
const INSUL: &str = "\u{423}\u{442}\u{435}\u{43f}\u{43b}\u{438}\u{442}\u{435}\u{43b}\u{44c}";
const BRICK: &str = "\u{41a}\u{438}\u{440}\u{43f}\u{438}\u{447}";
/// "Living room", "Kitchen", "Bedroom", "Bathroom", "Hall"
const LIVING: &str = "\u{413}\u{43e}\u{441}\u{442}\u{438}\u{43d}\u{430}\u{44f}";
const KITCHEN: &str = "\u{41a}\u{443}\u{445}\u{43d}\u{44f}";
const BEDROOM: &str = "\u{421}\u{43f}\u{430}\u{43b}\u{44c}\u{43d}\u{44f}";
const BATH: &str = "\u{412}\u{430}\u{43d}\u{43d}\u{430}\u{44f}";
const HALL: &str = "\u{41f}\u{440}\u{438}\u{445}\u{43e}\u{436}\u{430}\u{44f}";
/// "Long room name with a yo and <<quotes>> " (repeated past 300 characters)
const LONG: &str = "\u{414}\u{43b}\u{438}\u{43d}\u{43d}\u{43e}\u{435} \u{438}\u{43c}\u{44f} \
    \u{43f}\u{43e}\u{43c}\u{435}\u{449}\u{435}\u{43d}\u{438}\u{44f} \u{441} \u{451}\u{43b}\u{43a}\
    \u{43e}\u{439} \u{438} \u{ab}\u{43a}\u{430}\u{432}\u{44b}\u{447}\u{43a}\u{430}\u{43c}\u{438}\
    \u{bb} ";

const W: f64 = 12.0;
const D: f64 = 9.0;
const H: f64 = 3.0;

pub fn build(m: &mut Model) {
    m.set_project_info(&ProjectInfo {
        // "Residential house", "Lenin st., 1"
        name: "\u{416}\u{438}\u{43b}\u{43e}\u{439} \u{434}\u{43e}\u{43c}".into(),
        number: "2026-\u{416}-01".into(),
        address: "\u{443}\u{43b}. \u{41b}\u{435}\u{43d}\u{438}\u{43d}\u{430}, \u{434}. 1".into(),
        client: long_text(LONG, 200),
        ..Default::default()
    })
    .unwrap();
    let l1 = m.add_level(LEVELS[0], 0.0).unwrap();
    let l2 = m.add_level(LEVELS[1], H).unwrap();
    // "Exterior wall 510", "Partition 120", a type name of 120 characters
    let ext = wall_type(
        m,
        "\u{41d}\u{430}\u{440}\u{443}\u{436}\u{43d}\u{430}\u{44f} \
         \u{441}\u{442}\u{435}\u{43d}\u{430} 510",
        &[(PLASTER, 0.02), (AERATED, 0.4), (INSUL, 0.08), (BRICK, 0.01)],
    );
    let part = wall_type(
        m,
        "\u{41f}\u{435}\u{440}\u{435}\u{433}\u{43e}\u{440}\u{43e}\u{434}\u{43a}\u{430} 120",
        &[(BRICK, 0.12)],
    );
    let long_mat = long_text(LONG, 120);
    let long_t = wall_type(m, &long_text(LONG, 120), &[(long_mat.as_str(), 0.15)]);
    // "Floor slab 220", "Roofing"
    let floor = slab_type(
        m,
        "\u{41f}\u{43b}\u{438}\u{442}\u{430} \u{43f}\u{435}\u{440}\u{435}\u{43a}\u{440}\u{44b}\
         \u{442}\u{438}\u{44f} 220",
        &[(PLASTER, 0.02), ("concrete", 0.2)],
    );
    let roof_t = slab_type(m, "\u{41a}\u{440}\u{43e}\u{432}\u{43b}\u{44f}", &[("wood", 0.25)]);

    let mut house = vec![];
    for &lv in &[l1, l2] {
        // south facade in three parts: wall, curtain wall (4..8), wall
        let ws = vec![
            wall(lv, ext, (0.0, 0.0), (4.0, 0.0), H),
            wall(lv, ext, (8.0, 0.0), (W, 0.0), H),
            wall(lv, ext, (W, 0.0), (W, D), H),
            wall(lv, ext, (W, D), (0.0, D), H),
            wall(lv, ext, (0.0, D), (0.0, 0.0), H),
            wall(lv, part, (6.0, 0.0), (6.0, D), H),
            wall(lv, part, (0.0, 5.0), (6.0, 5.0), H),
            wall(lv, long_t, (6.0, 6.0), (W, 6.0), H),
        ];
        house.push(m.add_walls(&ws).unwrap());
        m.add_curtain_wall(&CurtainSpec {
            level: lv,
            // "Glazing"
            name: "\u{412}\u{438}\u{442}\u{440}\u{430}\u{436}".into(),
            a: (4.0, 0.0),
            b: (8.0, 0.0),
            bulge: 0.0,
            base_offset: 0.0,
            height: H,
            u: GridLines::Spacing(1.0),
            v: GridLines::Spacing(1.5),
            interior: MullionSection::Rect { w: 0.05, d: 0.12 },
            border: MullionSection::Rect { w: 0.06, d: 0.15 },
            lines: Default::default(),
            top: vec![],
        })
        .unwrap();
    }
    for w in &house {
        // east facade windows, north door and windows, doors in the partitions
        m.add_opening(w[2], &window(1.5, 1.2, 0.9, 1.4)).unwrap();
        m.add_opening(w[2], &window(7.0, 1.2, 0.9, 1.4)).unwrap();
        m.add_opening(w[3], &window(2.0, 1.5, 0.9, 1.4)).unwrap();
        m.add_opening(w[3], &window(8.5, 1.5, 0.9, 1.4)).unwrap();
        m.add_opening(w[5], &door(6.5, 0.9)).unwrap();
        m.add_opening(w[6], &door(3.0, 0.8)).unwrap();
        m.add_opening(w[7], &door(2.0, 0.8)).unwrap();
    }
    // entrance: north wall runs from x 12 to 0, so along 4.2..5.2 is x 7.8..6.8
    m.add_opening(house[0][3], &door(4.2, 1.0)).unwrap();

    let rooms = [
        (l1, LIVING.to_string(), "101", (3.0, 2.5)),
        (l1, KITCHEN.to_string(), "102", (3.0, 7.0)),
        (l1, HALL.to_string(), "103", (9.0, 3.0)),
        (l1, BATH.to_string(), "104", (9.0, 7.5)),
        (l2, BEDROOM.to_string(), "201", (3.0, 2.5)),
        (l2, format!("{BEDROOM} 2"), "202", (3.0, 7.0)),
        (l2, long_text(LONG, 300), "203", (9.0, 3.0)),
        (l2, BATH.to_string(), "204", (9.0, 7.5)),
    ];
    let specs: Vec<RoomSpec> = rooms
        .iter()
        .map(|(level, name, number, point)| RoomSpec {
            level: *level,
            name: name.clone(),
            number: number.to_string(),
            point: *point,
            height: H - 0.3,
        })
        .collect();
    let room_ids = m.add_rooms(&specs).unwrap();

    let mut slabs = vec![];
    for &lv in &[l1, l2] {
        slabs.push(
            m.add_slab(&SlabSpec {
                level: lv,
                slab_type: floor,
                offset: 0.0,
                outer: rect(0.0, 0.0, W, D),
                holes: vec![],
            })
            .unwrap(),
        );
    }
    m.add_roof(&RoofSpec {
        level: l2,
        slab_type: roof_t,
        offset: H,
        overhang: 0.6,
        fascia: 0.2,
        kind: RoofKind::Footprint {
            ring: rect(0.0, 0.0, W, D),
            slopes: vec![None, Some(35.0), None, Some(35.0)],
            mansard: None,
        },
    })
    .unwrap();

    families(m, l1, l2, &house, &slabs);
    phases(m, &house);
    params(m, ext, &room_ids);

    // "Floor plans"
    let sheet = m
        .add_sheet(&SheetDef::new(
            "\u{410}\u{420}-1",
            "\u{41f}\u{43b}\u{430}\u{43d}\u{44b} \u{44d}\u{442}\u{430}\u{436}\u{435}\u{439}",
        ))
        .unwrap();
    for (k, &lv) in [l1, l2].iter().enumerate() {
        m.add_viewport(&ViewportDef {
            sheet,
            view: ViewRef::Plan {
                level: lv,
                cut: DEFAULT_CUT,
            },
            scale: 100.0,
            center: (80.0 + 130.0 * k as f64, 160.0),
            crop: None,
            title: LEVELS[k].into(),
        })
        .unwrap();
    }
    m.add_schedule_place(&SchedulePlace {
        sheet,
        schedule: "rooms".into(),
        pos: (300.0, 280.0),
    })
    .unwrap();
}

/// Library families with a new Cyrillic type, instance values, and wall, floor and free hosts.
fn families(m: &mut Model, l1: i64, l2: i64, house: &[Vec<i64>], slabs: &[i64]) {
    let table_family = m.load_library_family("Table").unwrap();
    // "Dining table 1800x900"
    let dining = m
        .add_comp_type(
            table_family,
            "\u{421}\u{442}\u{43e}\u{43b} \u{43e}\u{431}\u{435}\u{434}\u{435}\u{43d}\u{43d}\u{44b}\
             \u{439} 1800x900",
            &[
                ("Width".to_string(), ParamValue::Num(1.8)),
                ("Depth".to_string(), ParamValue::Num(0.9)),
            ],
        )
        .unwrap();
    let chair = lib_type(m, "Chair", "450x500");
    let bed = lib_type(m, "Bed", "Double 1600x2000");
    let basin = lib_type(m, "Washbasin", "600");
    let wardrobe = lib_type(m, "Wardrobe", "1500x600x2200");
    let mut specs = vec![CompSpec::at(l1, dining, 3.0, 7.0)];
    for (k, (dx, dy)) in [(-0.6, -0.7), (0.6, -0.7), (-0.6, 0.7), (0.6, 0.7)].iter().enumerate() {
        let mut c = CompSpec::at(l1, chair, 3.0 + dx, 7.0 + dy);
        c.rotation = if *dy < 0.0 { 0.0 } else { std::f64::consts::PI };
        c.mirror = k % 2 == 1;
        specs.push(c);
    }
    let mut b = CompSpec::at(l2, bed, 3.0, 2.5);
    b.host = CompHost::Floor(slabs[1]);
    b.rotation = 0.5 * std::f64::consts::PI;
    specs.push(b);
    let mut wr = CompSpec::at(l2, wardrobe, 1.0, 4.4);
    wr.rotation = std::f64::consts::PI;
    specs.push(wr);
    for (k, w) in house.iter().enumerate() {
        // east wall (W,0)->(W,D): along 6.5 is in the bathroom, clear of the window at 7.0
        let mut s = CompSpec::at(if k == 0 { l1 } else { l2 }, basin, 6.5, 0.1);
        s.host = CompHost::Wall(w[2]);
        s.values = vec![("Rim".into(), ParamValue::Num(0.8 + 0.05 * k as f64))];
        specs.push(s);
    }
    m.add_components(&specs).unwrap();
}

/// The ground floor walls exist already; one partition is demolished in the new phase; a phase
/// "Reconstruction" of its own is added after the last one.
fn phases(m: &mut Model, house: &[Vec<i64>]) {
    let ph = m.phases().unwrap();
    let existing = ph
        .iter()
        .find(|p| p.key.as_deref() == Some("existing"))
        .map(|p| p.id)
        .unwrap_or(ph[0].id);
    let last = ph.last().map(|p| p.id).unwrap();
    let own = m
        .add_phase(
            "\u{420}\u{435}\u{43a}\u{43e}\u{43d}\u{441}\
             \u{442}\u{440}\u{443}\u{43a}\u{446}\u{438}\u{44f}",
            Some(last),
        )
        .unwrap();
    let ground: Vec<(&str, i64)> = house[0].iter().map(|&w| ("wall", w)).collect();
    m.set_element_phases(&ground, Some(existing), None).unwrap();
    // demolished with its door
    m.demolish(&[("wall", house[0][6])], last).unwrap();
    m.set_element_phases(&[("wall", house[1][7])], Some(own), None)
        .unwrap();
}

/// Cyrillic project parameters: a finish text per room, a fire rating per wall type, a cost in
/// roubles per component, a finish area per room.
fn params(m: &mut Model, ext: i64, rooms: &[i64]) {
    let spec = |name: &str, ptype, per_type, unit: &str, cat: &str| ParamSpec {
        name: name.into(),
        ptype,
        per_type,
        unit: unit.into(),
        group: String::new(),
        guid: None,
        categories: vec![cat.into()],
    };
    // "Finish", "Fire resistance", "Cost" (roubles), "Finish area"
    let finish = m
        .add_param(&spec(
            "\u{41e}\u{442}\u{434}\u{435}\u{43b}\u{43a}\u{430}",
            ParamType::Text,
            false,
            "",
            "room",
        ))
        .unwrap();
    let fire = m
        .add_param(&spec(
            "\u{41e}\u{433}\u{43d}\u{435}\u{441}\u{442}\u{43e}\
             \u{439}\u{43a}\u{43e}\u{441}\u{442}\u{44c}",
            ParamType::Text,
            true,
            "",
            "wall",
        ))
        .unwrap();
    let cost = m
        .add_param(&spec(
            "\u{421}\u{442}\u{43e}\u{438}\u{43c}\u{43e}\u{441}\u{442}\u{44c}",
            ParamType::Number,
            false,
            "\u{440}\u{443}\u{431}",
            "comp",
        ))
        .unwrap();
    let area = m
        .add_param(&spec(&long_text(LONG, 120), ParamType::Area, false, "", "room"))
        .unwrap();
    // "Wallpaper"
    let mut sets = vec![ParamSet {
        param: fire,
        kind: "wall_type".into(),
        id: ext,
        value: Some(PVal::Text("REI 150".into())),
    }];
    for (k, &r) in rooms.iter().enumerate() {
        sets.push(ParamSet {
            param: finish,
            kind: "room".into(),
            id: r,
            value: Some(PVal::Text(format!("\u{41e}\u{431}\u{43e}\u{438} \u{2116}{k}"))),
        });
        sets.push(ParamSet {
            param: area,
            kind: "room".into(),
            id: r,
            value: Some(PVal::Num(12.5 + k as f64)),
        });
    }
    let comps: Vec<i64> = {
        let mut st = m.sql().prepare("SELECT id FROM comp ORDER BY id").unwrap();
        st.query_map([], |r| r.get(0)).unwrap().map(|x| x.unwrap()).collect()
    };
    for (k, id) in comps.into_iter().enumerate() {
        sets.push(ParamSet {
            param: cost,
            kind: "comp".into(),
            id,
            value: Some(PVal::Num(1_000.0 * (k as f64 + 1.0) + 0.5)),
        });
    }
    m.set_param_values(&sets).unwrap();
}
