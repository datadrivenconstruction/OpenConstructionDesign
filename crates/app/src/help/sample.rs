// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The "Open sample" project: a two-storey 12 × 9 m house with all the main categories:
//! levels, axes, layered walls, doors, windows, a glazed wall, slabs with a stair opening, ceilings,
//! a roof, a stair with railings, terrace columns and beams, rooms, components, a dimension, marks,
//! a section and elevations, sheets, a duct network, a mass, a phase, a project parameter.
use ddc_model::{
    BeamJoin, BeamSpec, CeilingSpec, ColumnSpec, CompSpec, CurtainSpec, DimKind, DimRef, DimSpec,
    Error, Form, GridLines, GridSpec, Just, LayerSpec, MassSpec, MemberShape, MepDomain,
    MepSection, MepShape, Model, MullionSection, OpeningSpec, ParamSpec, ParamType, RailingHost,
    RailingSpec, RoofKind, RoofSpec, RoomSpec, RunSpec, SectionSpec, SheetDef, SlabSpec, StairSpec,
    SystemKind, TagHost, TagSpec, ViewKind, WallSpec,
};

/// House footprint along the outer wall axes, m.
pub const SIZE: (f64, f64) = (12.0, 9.0);
/// Storey height, m.
pub const STOREY: f64 = 3.0;

fn layers(ls: &[(&str, f64)]) -> Vec<LayerSpec> {
    ls.iter()
        .map(|(m, t)| LayerSpec {
            material: (*m).into(),
            thickness: *t,
        })
        .collect()
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<(f64, f64)> {
    vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
}

/// An optional part of the sample: an error does not stop the rest from opening (the test requires all of it).
fn soft<T>(what: &str, r: Result<T, Error>) -> Option<T> {
    r.map_err(|e| eprintln!("sample: {what}: {e}")).ok()
}

struct Floor {
    level: i64,
    /// Outer walls: south, east, north, west (counterclockwise).
    outer: [i64; 4],
    /// Inner walls: x = 6 (full depth), y = 5 on the left and right.
    inner: [i64; 3],
}

fn storey(m: &mut Model, level: i64, ext: i64, part: i64) -> Result<Floor, Error> {
    let (w, d) = SIZE;
    let wall = |a, b, t| WallSpec {
        a,
        b,
        level,
        base_offset: 0.0,
        height: STOREY,
        wall_type: t,
        offset0: None,
    };
    let c = [(0.0, 0.0), (w, 0.0), (w, d), (0.0, d)];
    let mut outer = [0; 4];
    for (i, o) in outer.iter_mut().enumerate() {
        *o = m.add_wall(&wall(c[i], c[(i + 1) % 4], ext))?;
    }
    let inner = [
        m.add_wall(&wall((6.0, 0.0), (6.0, d), part))?,
        m.add_wall(&wall((0.0, 5.0), (6.0, 5.0), part))?,
        m.add_wall(&wall((6.0, 5.0), (w, 5.0), part))?,
    ];
    Ok(Floor {
        level,
        outer,
        inner,
    })
}

fn opening(kind: &str, along: f64, width: f64, sill: f64, height: f64) -> OpeningSpec {
    OpeningSpec {
        along,
        width,
        sill,
        height,
        kind: kind.into(),
    }
}

/// Windows on the elevations (by `along` from the wall start) and interior doors; `entrance` is the entrance door.
fn openings(m: &mut Model, f: &Floor, entrance: bool) -> Result<Vec<i64>, Error> {
    let win = |along: f64| opening("window", along, 1.4, 0.9, 1.5);
    let door = |along: f64| opening("door", along, 0.9, 0.0, 2.1);
    let [s, e, n, w] = f.outer;
    let mut out = vec![];
    for (wall, o) in [
        (s, win(1.5)),
        (s, win(8.5)),
        (e, win(1.5)),
        (e, win(6.5)),
        (n, win(1.5)),
        (n, win(8.0)),
        (w, win(1.0)),
        (w, win(6.0)),
        (f.inner[0], door(1.5)),
        (f.inner[0], door(6.5)),
        (f.inner[1], door(3.5)),
        (f.inner[2], door(1.0)),
    ] {
        out.push(m.add_opening(wall, &o)?);
    }
    if entrance {
        out.push(m.add_opening(s, &opening("door", 4.0, 1.0, 0.0, 2.2))?);
    }
    Ok(out)
}

/// The sample in the UI language (`ru`): names of levels, types, rooms, views, sheets.
pub fn build(ru: bool) -> Result<Model, Error> {
    let t = |r: &'static str, e: &'static str| if ru { r } else { e };
    let (w, d) = SIZE;
    let mut m = Model::open(":memory:")?;
    let l1 = m.add_level(t("\u{42d}\u{442}\u{430}\u{436} 1", "Level 1"), 0.0)?;
    let l2 = m.add_level(t("\u{42d}\u{442}\u{430}\u{436} 2", "Level 2"), STOREY)?;

    let ext = m.add_wall_type(
        t("\u{41d}\u{430}\u{440}\u{443}\u{436}\u{43d}\u{430}\u{44f} 380", "Exterior 380"),
        &layers(&[
            ("plaster", 0.02),
            ("masonry", 0.25),
            ("insulation", 0.1),
            ("render", 0.01),
        ]),
    )?;
    let part = m.add_wall_type(
        t("\u{41f}\u{435}\u{440}\u{435}\u{433}\u{43e}\u{440}\u{43e}\u{434}\u{43a}\u{430} 120", "Partition 120"),
        &layers(&[("gypsum", 0.12)]),
    )?;
    let floor_t = m.add_slab_type(
        t("\u{41f}\u{435}\u{440}\u{435}\u{43a}\u{440}\u{44b}\u{442}\u{438}\u{435} 250", "Floor 250"),
        &layers(&[("screed", 0.05), ("concrete", 0.2)]),
    )?;
    let roof_t = m.add_slab_type(
        t("\u{41a}\u{440}\u{43e}\u{432}\u{43b}\u{44f} 300", "Roof 300"),
        &layers(&[("roofing", 0.02), ("insulation", 0.2), ("timber", 0.08)]),
    )?;
    let ceil_t = m.add_slab_type(
        t("\u{41f}\u{43e}\u{442}\u{43e}\u{43b}\u{43e}\u{43a} \u{413}\u{41a}\u{41b}", "Gypsum ceiling"),
        &layers(&[("gypsum", 0.0125)]),
    )?;

    // axes: digits by x, letters by y
    for (name, x) in [("1", 0.0), ("2", 6.0), ("3", w), ("4", 15.0)] {
        m.add_grid(&GridSpec::line(name, (x, -4.0), (x, d + 2.0)))?;
    }
    for (name, y) in [("A", 0.0), ("B", 5.0), ("C", d)] {
        m.add_grid(&GridSpec::line(name, (-2.0, y), (17.0, y)))?;
    }

    let f1 = storey(&mut m, l1, ext, part)?;
    let f2 = storey(&mut m, l2, ext, part)?;
    let o1 = openings(&mut m, &f1, true)?;
    openings(&mut m, &f2, false)?;

    // slabs: the ground floor and the second-floor slab with a stair opening
    let stair_hole = rect(0.6, 5.3, 5.7, 7.8);
    m.add_slab(&SlabSpec {
        level: l1,
        slab_type: floor_t,
        offset: 0.0,
        outer: rect(0.0, 0.0, w, d),
        holes: vec![],
    })?;
    m.add_slab(&SlabSpec {
        level: l2,
        slab_type: floor_t,
        offset: 0.0,
        outer: rect(0.0, 0.0, w, d),
        holes: vec![stair_hole],
    })?;

    let stair = m.add_stair(&StairSpec::straight(l1, l2, (0.9, 6.5), 0.0))?;
    m.add_stair_railings(stair, &RailingSpec::new(l1, RailingHost::Path(vec![])))?;

    let names1 = [
        (t("\u{413}\u{43e}\u{441}\u{442}\u{438}\u{43d}\u{430}\u{44f}", "Living room"), "101", (3.0, 2.5)),
        (t("\u{425}\u{43e}\u{43b}\u{43b}", "Hall"), "102", (3.0, 8.4)),
        (t("\u{41a}\u{443}\u{445}\u{43d}\u{44f}", "Kitchen"), "103", (9.0, 2.5)),
        (t("\u{41a}\u{430}\u{431}\u{438}\u{43d}\u{435}\u{442}", "Study"), "104", (9.0, 7.0)),
    ];
    let names2 = [
        (t("\u{421}\u{43f}\u{430}\u{43b}\u{44c}\u{43d}\u{44f}", "Bedroom"), "201", (3.0, 2.5)),
        (t("\u{425}\u{43e}\u{43b}\u{43b}", "Landing"), "202", (3.0, 8.4)),
        (t("\u{414}\u{435}\u{442}\u{441}\u{43a}\u{430}\u{44f}", "Kids room"), "203", (9.0, 2.5)),
        (t("\u{412}\u{430}\u{43d}\u{43d}\u{430}\u{44f}", "Bathroom"), "204", (9.0, 7.0)),
    ];
    let mut rooms = vec![];
    for (level, names) in [(l1, names1), (l2, names2)] {
        for (name, number, point) in names {
            rooms.push(m.add_room(&RoomSpec {
                level,
                name: name.into(),
                number: number.into(),
                point,
                height: 2.7,
            })?);
        }
    }
    // ceilings in the rooms by the south elevation
    for level in [l1, l2] {
        for (x0, x1) in [(0.2, 5.9), (6.1, w - 0.2)] {
            m.add_ceiling(&CeilingSpec {
                level,
                slab_type: ceil_t,
                height: 2.7,
                outer: rect(x0, 0.2, x1, 4.9),
                holes: vec![],
            })?;
        }
    }

    // a gable roof: slopes to the south and north, gables on the east and west
    m.add_roof(&RoofSpec {
        level: l2,
        slab_type: roof_t,
        offset: STOREY,
        overhang: 0.5,
        fascia: 0.2,
        kind: RoofKind::Footprint {
            ring: rect(0.0, 0.0, w, d),
            slopes: vec![Some(30.0), None, Some(30.0), None],
            mansard: None,
        },
    })?;

    // a terrace on the east: columns on axis 4 and beams along the top
    let col_t = m.add_member_type(
        t("\u{41a}\u{43e}\u{43b}\u{43e}\u{43d}\u{43d}\u{430} 300×300", "Column 300x300"),
        &MemberShape::Rect { b: 0.3, h: 0.3 },
        "Concrete",
    )?;
    let beam_t = m.add_member_type(
        t("\u{411}\u{430}\u{43b}\u{43a}\u{430} 200×400", "Beam 200x400"),
        &MemberShape::Rect { b: 0.2, h: 0.4 },
        "Concrete",
    )?;
    let ys = [0.0, 4.5, d];
    for y in ys {
        m.add_column(&ColumnSpec {
            level: l1,
            top_level: Some(l2),
            member_type: col_t,
            structural: true,
            at: (15.0, y),
            base_offset: 0.0,
            top_offset: 0.0,
            rotation: 0.0,
            top_at: None,
        })?;
    }
    for (a, b) in [(0.0, 4.5), (4.5, d)] {
        m.add_beam(&BeamSpec {
            level: l2,
            member_type: beam_t,
            a: (15.0, a, 0.0),
            b: (15.0, b, 0.0),
            bulge: 0.0,
            yjust: Just::default(),
            zjust: Just::default(),
            rotation: 0.0,
            start_ext: 0.0,
            end_ext: 0.0,
            join_start: BeamJoin::default(),
            join_end: BeamJoin::default(),
        })?;
    }

    // a dimension along the south elevation and marks for rooms and the entrance door
    m.add_dim(&DimSpec {
        level: l1,
        kind: DimKind::Linear,
        refs: vec![
            DimRef::AxisEnd {
                wall: f1.outer[0],
                end: 0,
            },
            DimRef::AxisEnd {
                wall: f1.outer[0],
                end: 1,
            },
        ],
        dir: (0.0, 0.0),
        side: -1.0,
        offset: 1.5,
        tier: 0,
        auto: false,
    })?;
    let mut tags: Vec<TagSpec> = rooms
        .iter()
        .map(|&r| TagSpec {
            level: if rooms[..4].contains(&r) { l1 } else { l2 },
            host: TagHost::Room(r),
            offset: (0.0, 0.0),
            leader: false,
        })
        .collect();
    if let Some(&door) = o1.last() {
        tags.push(TagSpec {
            level: l1,
            host: TagHost::Opening(door),
            offset: (0.0, -0.8),
            leader: false,
        });
    }
    m.add_tags(&tags)?;

    m.add_view_def(&SectionSpec {
        kind: ViewKind::Section,
        name: t("\u{420}\u{430}\u{437}\u{440}\u{435}\u{437} 1", "Section 1").into(),
        a: (3.0, -3.0),
        b: (3.0, d + 3.0),
        depth: 10.0,
        far_clip: false,
    })?;
    m.add_compass_elevations(4.0)?;
    m.add_plan_sheets(
        &SheetDef::new("A-101", t("\u{41f}\u{43b}\u{430}\u{43d} \u{44d}\u{442}\u{430}\u{436}\u{430}", "Floor plan")),
        &[l1, l2],
        100.0,
        (200.0, 160.0),
    )?;

    // the veranda glazed wall in front of the south elevation
    let mull = MullionSection::Rect { w: 0.05, d: 0.12 };
    soft(
        "curtain",
        m.add_curtain_wall(&CurtainSpec {
            level: l1,
            name: t("\u{412}\u{438}\u{442}\u{440}\u{430}\u{436} \u{432}\u{435}\u{440}\u{430}\u{43d}\u{434}\u{44b}", "Porch glazing").into(),
            a: (2.0, -2.5),
            b: (10.0, -2.5),
            bulge: 0.0,
            base_offset: 0.0,
            height: 2.7,
            u: GridLines::Spacing(1.6),
            v: GridLines::Spacing(1.35),
            interior: mull,
            border: mull,
            lines: Default::default(),
            top: vec![],
        }),
    );

    // furniture from the built-in library
    for (level, family, ty, at) in [
        (l1, "Table", "1600x800", (3.0, 2.5)),
        (l1, "Chair", "450x500", (3.0, 1.6)),
        (l2, "Bed", "Double 1600x2000", (3.0, 2.0)),
    ] {
        let ct = soft("family", m.load_library_family(family))
            .and_then(|f| soft("comp type", m.comp_type_id(f, ty)).flatten());
        if let Some(ct) = ct {
            soft(
                "component",
                m.add_component(&CompSpec::at(level, ct, at.0, at.1)),
            );
        }
    }

    // a supply duct under the kitchen ceiling
    if let Some(sys) = soft(
        "mep system",
        m.add_mep_system(t("\u{41f}\u{440}\u{438}\u{442}\u{43e}\u{43a} \u{41f}1", "Supply S1"), SystemKind::SupplyAir),
    ) {
        let sec = MepSection::new(MepDomain::Duct, MepShape::Rect { w: 0.4, h: 0.25 });
        soft(
            "duct",
            m.add_mep_run(&RunSpec::new(
                sys,
                l1,
                sec,
                vec![(6.5, 1.0, 2.45), (11.0, 1.0, 2.45), (11.0, 4.0, 2.45)],
            )),
        );
    }

    soft(
        "mass",
        m.add_mass(&MassSpec {
            level: l1,
            name: t("\u{424}\u{43e}\u{440}\u{43c}\u{430} \u{433}\u{430}\u{440}\u{430}\u{436}\u{430}", "Garage mass").into(),
            void: false,
            inplace: false,
            form: Form::Cuboid {
                lo: (18.0, 0.0, 0.0),
                hi: (24.0, 6.0, 3.0),
            },
        }),
    );
    soft(
        "phase",
        m.add_phase(t("\u{411}\u{43b}\u{430}\u{433}\u{43e}\u{443}\u{441}\u{442}\u{440}\u{43e}\u{439}\u{441}\u{442}\u{432}\u{43e}", "Landscaping"), None),
    );
    soft(
        "param",
        m.add_param(&ParamSpec {
            name: t("\u{41e}\u{433}\u{43d}\u{435}\u{441}\u{442}\u{43e}\u{439}\u{43a}\u{43e}\u{441}\u{442}\u{44c}", "Fire rating").into(),
            ptype: ParamType::Text,
            per_type: false,
            unit: String::new(),
            group: t("\u{41f}\u{440}\u{43e}\u{447}\u{435}\u{435}", "Other").into(),
            guid: None,
            categories: vec!["wall".into()],
        }),
    );
    Ok(m)
}

/// The sample's first (lowest) level, which the plan shows after opening.
pub fn first_level(m: &Model) -> Option<i64> {
    m.sql()
        .query_row(
            "SELECT id FROM level ORDER BY elevation, id LIMIT 1",
            [],
            |r| r.get(0),
        )
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(m: &Model, sql: &str) -> i64 {
        m.sql()
            .query_row(sql, [], |r| r.get::<_, i64>(0))
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
    }

    #[test]
    fn sample_is_consistent() {
        for ru in [true, false] {
            let m = build(ru).unwrap();
            let issues = m.check_integrity().unwrap();
            assert!(issues.is_empty(), "ru={ru}: {issues:?}");
        }
    }

    #[test]
    fn sample_has_every_category() {
        let m = build(true).unwrap();
        for (sql, min) in [
            ("SELECT COUNT(*) FROM level", 2),
            ("SELECT COUNT(*) FROM grid", 7),
            ("SELECT COUNT(*) FROM wall", 14),
            ("SELECT COUNT(*) FROM opening WHERE kind='door'", 9),
            ("SELECT COUNT(*) FROM opening WHERE kind='window'", 16),
            ("SELECT COUNT(*) FROM curtain_wall", 1),
            ("SELECT COUNT(*) FROM slab", 2),
            ("SELECT COUNT(*) FROM ceiling", 4),
            ("SELECT COUNT(*) FROM roof", 1),
            ("SELECT COUNT(*) FROM stair", 1),
            ("SELECT COUNT(*) FROM railing", 2),
            ("SELECT COUNT(*) FROM col", 3),
            ("SELECT COUNT(*) FROM beam", 2),
            ("SELECT COUNT(*) FROM room", 8),
            ("SELECT COUNT(*) FROM comp", 3),
            ("SELECT COUNT(*) FROM dim", 1),
            ("SELECT COUNT(*) FROM view_def", 5),
            ("SELECT COUNT(*) FROM sheet", 2),
            ("SELECT COUNT(*) FROM mep_run", 1),
            ("SELECT COUNT(*) FROM mass", 1),
            ("SELECT COUNT(*) FROM pparam", 1),
        ] {
            let n = count(&m, sql);
            assert!(n >= min, "{sql}: {n} < {min}");
        }
        let l1 = first_level(&m).unwrap();
        assert!(!m.tags(l1).unwrap().is_empty());
    }

    #[test]
    fn sample_opens_in_editor() {
        let mut ed = crate::editor::Editor::new_in_memory().unwrap();
        let m = build(true).unwrap();
        ed.level = first_level(&m).unwrap();
        ed.replace_model(m).unwrap();
        assert_eq!(ed.walls.len(), 7);
        assert!(!ed.openings.is_empty() && !ed.rooms.is_empty());
    }
}
