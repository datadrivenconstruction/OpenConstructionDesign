// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A small office building for drawing set tests: `floors` storeys of 18 x 12 m on axes 1-4
//! and A-C, brick walls on the axes, six rooms, seven doors and ten windows a floor, slabs,
//! straight stairs between floors and project information for the title block.

use ddc_model::{
    GridSpec, LayerSpec, Model, OpeningSpec, ProjectInfo, RoomSpec, SlabSpec, StairSpec, Swing,
    TagHost, WallSpec,
};

pub const STOREY: f64 = 3.3;

fn layer(material: &str, thickness: f64) -> LayerSpec {
    LayerSpec {
        material: material.into(),
        thickness,
    }
}

fn opening(kind: &str, along: f64) -> OpeningSpec {
    let door = kind == "door";
    OpeningSpec {
        along,
        width: if door { 0.9 } else { 1.5 },
        sill: if door { 0.0 } else { 0.9 },
        height: if door { 2.1 } else { 1.5 },
        kind: kind.into(),
    }
}

/// Counts of what [`building`] made: per floor and in all.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Made {
    pub levels: Vec<i64>,
    pub doors: usize,
    pub windows: usize,
    pub rooms: usize,
    pub walls_per_level: usize,
    pub wall_type: i64,
}

pub fn building(floors: usize) -> Result<(Model, Made), ddc_model::Error> {
    let mut m = Model::open(":memory:")?;
    let made = build_into(&mut m, floors)?;
    Ok((m, made))
}

/// The building with opening marks by type (`D1`, `W1`), so its middle floors draw alike: a
/// typical floor for the drawing set.
pub fn typical_building(floors: usize) -> Result<(Model, Made), ddc_model::Error> {
    let mut m = Model::open(":memory:")?;
    let made = build(&mut m, floors, Marks::Typed)?;
    Ok((m, made))
}

/// The building with no door or window marks, as a model drawn without them comes.
pub fn unmarked_building(floors: usize) -> Result<(Model, Made), ddc_model::Error> {
    let mut m = Model::open(":memory:")?;
    let made = build(&mut m, floors, Marks::None)?;
    Ok((m, made))
}

/// [`unmarked_building`] in an existing model, beside what is there.
pub fn build_unmarked_into(m: &mut Model, floors: usize) -> Result<Made, ddc_model::Error> {
    build(m, floors, Marks::None)
}

/// The building in an existing model, beside what is there.
pub fn build_into(m: &mut Model, floors: usize) -> Result<Made, ddc_model::Error> {
    build(m, floors, Marks::Through)
}

/// Door and window marks: numbered through the building, by type, or none.
#[derive(Clone, Copy, PartialEq)]
enum Marks {
    Through,
    Typed,
    None,
}

fn build(m: &mut Model, floors: usize, marks: Marks) -> Result<Made, ddc_model::Error> {
    let wt = m.add_wall_type("Brick 380", &[layer("brick", 0.38)])?;
    let inner = m.add_wall_type("Brick 120", &[layer("brick", 0.12)])?;
    let st = m.add_slab_type("Slab 220", &[layer("concrete", 0.22)])?;
    for (n, x) in ["1", "2", "3", "4"].iter().zip([0.0, 6.0, 12.0, 18.0]) {
        m.add_grid(&GridSpec::line(n, (x, -2.5), (x, 14.5)))?;
    }
    for (n, y) in ["A", "B", "C"].iter().zip([0.0, 6.0, 12.0]) {
        m.add_grid(&GridSpec::line(n, (-2.5, y), (20.5, y)))?;
    }
    let mut made = Made {
        wall_type: wt,
        ..Made::default()
    };
    let mut num = 0;
    for f in 0..=floors {
        let lv = m.add_level(&format!("Level {}", f + 1), f as f64 * STOREY)?;
        made.levels.push(lv);
    }
    for f in 0..floors {
        let lv = made.levels[f];
        let wall = |a, b, t| WallSpec {
            a,
            b,
            level: lv,
            base_offset: 0.0,
            height: STOREY,
            wall_type: t,
            offset0: None,
        };
        // outer walls counter-clockwise, then the inner ones
        let specs = [
            wall((0.0, 0.0), (18.0, 0.0), wt),
            wall((18.0, 0.0), (18.0, 12.0), wt),
            wall((18.0, 12.0), (0.0, 12.0), wt),
            wall((0.0, 12.0), (0.0, 0.0), wt),
            wall((6.0, 0.0), (6.0, 12.0), inner),
            wall((12.0, 0.0), (12.0, 12.0), inner),
            wall((0.0, 6.0), (18.0, 6.0), inner),
        ];
        let ids = m.add_walls(&specs)?;
        made.walls_per_level = ids.len();
        let mut put =
            |m: &mut Model, w: i64, kind: &str, along: f64| -> Result<(), ddc_model::Error> {
                let id = m.add_opening_with(w, &opening(kind, along), Swing::default())?;
                num += 1;
                let mark = if kind == "door" { "D" } else { "W" };
                let n = if marks == Marks::Typed { 1 } else { num };
                if marks != Marks::None {
                    m.set_mark(TagHost::Opening(id), &format!("{mark}{n}"))?;
                }
                Ok(())
            };
        for (w, al) in [
            (0, 2.25),
            (0, 8.25),
            (0, 14.25),
            (2, 2.25),
            (2, 8.25),
            (2, 14.25),
        ] {
            put(m, ids[w], "window", al)?;
        }
        for (w, al) in [(1, 2.25), (1, 8.25), (3, 2.25), (3, 8.25)] {
            put(m, ids[w], "window", al)?;
        }
        made.windows += 10;
        for (w, al) in [
            (4, 3.5),
            (4, 9.5),
            (5, 3.5),
            (5, 9.5),
            (6, 4.0),
            (6, 9.0),
            (6, 16.0),
        ] {
            put(m, ids[w], "door", al)?;
        }
        made.doors += 7;
        for (k, (x, y)) in [
            (3.0, 3.0),
            (9.0, 3.0),
            (15.0, 3.0),
            (3.0, 9.0),
            (9.0, 9.0),
            (15.0, 9.0),
        ]
        .into_iter()
        .enumerate()
        {
            m.add_room(&RoomSpec {
                level: lv,
                name: if k == 0 {
                    "Stair hall".into()
                } else {
                    "Office".into()
                },
                number: format!("{}{:02}", f + 1, k + 1),
                point: (x + 0.4, y + 0.6),
                height: STOREY - 0.3,
            })?;
            made.rooms += 1;
        }
        m.add_slab(&SlabSpec {
            level: lv,
            slab_type: st,
            offset: 0.0,
            outer: vec![
                (-0.19, -0.19),
                (18.19, -0.19),
                (18.19, 12.19),
                (-0.19, 12.19),
            ],
            holes: vec![],
        })?;
        if f + 1 < floors {
            let top = made.levels[f + 1];
            let up = std::f64::consts::FRAC_PI_2;
            m.add_stair(&StairSpec::straight(lv, top, (2.0, 0.5), up))?;
        }
    }
    m.set_project_info(&ProjectInfo {
        name: "Office building".into(),
        number: "2026-11-OB".into(),
        organization: "Design bureau".into(),
        author: "Author".into(),
        checker: "Checker".into(),
        stage: "P".into(),
        ..Default::default()
    })?;
    Ok(made)
}
