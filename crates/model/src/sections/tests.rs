// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use crate::curtain::{CurtainSpec, GridLines};
use crate::{
    LayerSpec, MullionSection, OpeningSpec, RoomSpec, SlabSlope, SlabSpec, Swing, WallSpec,
};

fn layer(m: &str, t: f64) -> Vec<LayerSpec> {
    vec![LayerSpec {
        material: m.into(),
        thickness: t,
    }]
}

/// Sample house: walls 6x4 (Brick 200 inward, 3 m), a door on the south, a window on the north,
/// a Concrete 200 floor, a sloped roof on L2, a curtain wall at y = 2, room 101 south of it.
pub(crate) fn house() -> (Model, Vec<i64>) {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let l2 = m.add_level("L2", 3.0).unwrap();
    let wt = m.add_wall_type("W", &layer("Brick", 0.2)).unwrap();
    let c = [(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)];
    let specs: Vec<WallSpec> = (0..4)
        .map(|k| WallSpec {
            a: c[k],
            b: c[(k + 1) % 4],
            level: l1,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: Some(0.0),
        })
        .collect();
    let ids = m.add_walls(&specs).unwrap();
    let op = |along, width, sill, height, kind: &str| OpeningSpec {
        along,
        width,
        sill,
        height,
        kind: kind.into(),
    };
    let door = m
        .add_opening_with(ids[0], &op(1.0, 0.9, 0.0, 2.1, "door"), Swing::default())
        .unwrap();
    let window = m
        .add_opening(ids[2], &op(2.0, 1.5, 0.9, 1.5, "window"))
        .unwrap();
    let st = m.add_slab_type("S", &layer("Concrete", 0.2)).unwrap();
    let outer = c.to_vec();
    let floor = m
        .add_slab(&SlabSpec {
            level: l1,
            slab_type: st,
            offset: 0.0,
            outer: outer.clone(),
            holes: vec![],
        })
        .unwrap();
    let roof = m
        .add_slab(&SlabSpec {
            level: l2,
            slab_type: st,
            offset: 0.2,
            outer,
            holes: vec![],
        })
        .unwrap();
    m.set_slab_slope(
        roof,
        Some(SlabSlope {
            p0: v2(0.0, 0.0),
            gx: 0.0,
            gy: 0.25,
        }),
    )
    .unwrap();
    let mu = MullionSection::Rect { w: 0.05, d: 0.1 };
    let cw = m
        .add_curtain_wall(&CurtainSpec {
            level: l1,
            name: "CW".into(),
            a: (0.2, 2.0),
            b: (5.8, 2.0),
            bulge: 0.0,
            base_offset: 0.0,
            height: 3.0,
            u: GridLines::Spacing(1.0),
            v: GridLines::Spacing(1.5),
            interior: mu,
            border: mu,
            lines: Default::default(),
            top: vec![],
        })
        .unwrap();
    m.add_room(&RoomSpec {
        level: l1,
        name: "Office".into(),
        number: "101".into(),
        point: (3.0, 1.0),
        height: 3.0,
    })
    .unwrap();
    (m, vec![ids[0], ids[2], door, window, floor, roof, cw])
}

pub(crate) fn grids() -> Vec<GridAxis> {
    vec![
        GridAxis {
            label: "1".into(),
            a: (-1.0, 0.0),
            b: (7.0, 0.0),
        },
        GridAxis {
            label: "A".into(),
            a: (0.0, -1.0),
            b: (0.0, 5.0),
        },
    ]
}

fn area(r: &[P]) -> f64 {
    let mut s = 0.0;
    for i in 0..r.len() {
        let (p, q) = (r[i], r[(i + 1) % r.len()]);
        s += p.0 * q.1 - q.0 * p.1;
    }
    s / 2.0
}

pub(crate) fn cut_spec() -> SectionSpec {
    SectionSpec {
        kind: ViewKind::Section,
        name: "1-1".into(),
        a: (1.45, -1.0),
        b: (1.45, 5.0),
        depth: 10.0,
        far_clip: false,
    }
}

type CutRow<'a> = (i64, &'a str, f64, [f64; 4]);

fn cuts(v: &PlanView) -> Vec<CutRow<'_>> {
    v.prims
        .iter()
        .filter_map(|p| match p {
            ViewPrim::SectionCut {
                element,
                material,
                rings,
                holes,
                ..
            } => {
                assert!(holes.is_empty());
                let mut bb = [f64::INFINITY, f64::INFINITY, -f64::INFINITY, -f64::INFINITY];
                for q in rings {
                    bb = [
                        bb[0].min(q.0),
                        bb[1].min(q.1),
                        bb[2].max(q.0),
                        bb[3].max(q.1),
                    ];
                }
                Some((*element, material.as_str(), area(rings), bb))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn section_through_door_exact_cuts() {
    let (m, ids) = house();
    let (south, north, door, floor, roof, cw) = (ids[0], ids[1], ids[2], ids[4], ids[5], ids[6]);
    let v = m.section_view(&cut_spec(), &grids()).unwrap();
    let cuts = cuts(&v);
    assert_eq!(cuts.len(), 11, "{cuts:?}");
    let total: f64 = cuts.iter().map(|c| c.2).sum();
    assert!((total - 2.55325).abs() < 1e-9, "{total}");
    let find = |el: i64, mat: &str| -> Vec<(f64, [f64; 4])> {
        cuts.iter()
            .filter(|c| c.0 == el && c.1 == mat)
            .map(|c| (c.2, c.3))
            .collect()
    };
    let near = |a: [f64; 4], b: [f64; 4]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9);
    let s = find(south, "Brick");
    assert_eq!(s.len(), 1);
    assert!(
        (s[0].0 - 0.18).abs() < 1e-12 && near(s[0].1, [1.0, 2.1, 1.2, 3.0]),
        "{s:?}"
    );
    let n = find(north, "Brick");
    assert!(
        (n[0].0 - 0.6).abs() < 1e-12 && near(n[0].1, [4.8, 0.0, 5.0, 3.0]),
        "{n:?}"
    );
    let head = find(door, "Frame");
    assert_eq!(head.len(), 1);
    assert!((head[0].0 - 0.005).abs() < 1e-12 && near(head[0].1, [1.05, 2.05, 1.15, 2.1]));
    let leaf = find(door, "Door");
    assert!((leaf[0].0 - 0.082).abs() < 1e-12 && near(leaf[0].1, [1.08, 0.0, 1.12, 2.05]));
    let fl = find(floor, "Concrete");
    assert!((fl[0].0 - 0.8).abs() < 1e-12 && near(fl[0].1, [1.0, -0.2, 5.0, 0.0]));
    let rf = find(roof, "Concrete");
    assert!(
        (rf[0].0 - 0.8).abs() < 1e-12 && near(rf[0].1, [1.0, 3.0, 5.0, 4.2]),
        "{rf:?}"
    );
    let panels = find(cw, "Glass");
    assert_eq!(panels.len(), 2);
    for p in &panels {
        assert!((p.0 - 0.035625).abs() < 1e-12, "{p:?}");
    }
    let bars = find(cw, "Mullion");
    assert_eq!(bars.len(), 3);
    for b in &bars {
        assert!(
            (b.0 - 0.005).abs() < 1e-12 && (b.1[0] - 2.95).abs() < 1e-9,
            "{b:?}"
        );
    }
    assert_eq!(v.count(|p| matches!(p, ViewPrim::Level { .. })), 2);
    let dims: Vec<f64> = v
        .prims
        .iter()
        .filter_map(|p| match p {
            ViewPrim::Dim { value, .. } => Some(*value),
            _ => None,
        })
        .collect();
    assert_eq!(dims, vec![3.0]);
    let gs: Vec<(String, f64)> = v
        .prims
        .iter()
        .filter_map(|p| match p {
            ViewPrim::Grid { label, a, .. } => Some((label.clone(), a.0)),
            _ => None,
        })
        .collect();
    assert_eq!(gs.len(), 1);
    assert_eq!(gs[0].0, "1");
    assert!((gs[0].1 - 1.0).abs() < 1e-12);
    assert_eq!(v.count(|p| matches!(p, ViewPrim::RoomTag { .. })), 1);
    // Looking west: the west wall behind the section is in the "beyond the plane" class.
    assert!(v.prims.iter().any(|p| matches!(
        p,
        ViewPrim::Edge {
            class: LineClass::Beyond,
            ..
        }
    )));
    // Flipped view: the same cuts.
    let f = m.section_view(&cut_spec().flipped(), &grids()).unwrap();
    let fc: f64 = cuts_area(&f);
    assert!((fc - 2.55325).abs() < 1e-9, "{fc}");
}

fn cuts_area(v: &PlanView) -> f64 {
    cuts(v).iter().map(|c| c.2).sum()
}

fn edges_of(v: &PlanView, tb: &str, el: i64) -> f64 {
    v.prims
        .iter()
        .filter_map(|p| match p {
            ViewPrim::Edge {
                table,
                element,
                a,
                b,
                ..
            } if *element == el && *table == tb => {
                Some(((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt())
            }
            _ => None,
        })
        .sum()
}

#[test]
fn compass_elevations_count_and_visibility() {
    let (mut m, ids) = house();
    let (south, door, window, cw) = (ids[0], ids[2], ids[3], ids[6]);
    let made = m.add_compass_elevations(3.0).unwrap();
    assert_eq!(made.len(), 4);
    assert!(m.add_compass_elevations(3.0).unwrap().is_empty());
    let defs = m.view_defs().unwrap();
    let names: Vec<&str> = defs.iter().map(|d| d.1.name.as_str()).collect();
    assert_eq!(names, COMPASS.to_vec());
    let by = |n: &str| defs.iter().find(|d| d.1.name == n).unwrap().0;
    let s = m.draw_view_def(by("South"), &grids()).unwrap();
    let n = m.draw_view_def(by("North"), &grids()).unwrap();
    for v in [&s, &n] {
        assert_eq!(v.count(|p| matches!(p, ViewPrim::SectionCut { .. })), 0);
        assert_eq!(v.count(|p| matches!(p, ViewPrim::RoomTag { .. })), 0);
        assert!(v.prims.iter().all(|p| !matches!(
            p,
            ViewPrim::Edge {
                class: LineClass::Beyond,
                ..
            }
        )));
        assert_eq!(
            edges_of(v, "curtain_wall", cw),
            0.0,
            "curtain wall is behind solid walls"
        );
    }
    // South: outline of the south wall with a 22.2 m door cutout; the door is visible, the window is not.
    assert!(
        (edges_of(&s, "wall", south) - 22.2).abs() < 1e-9,
        "{}",
        edges_of(&s, "wall", south)
    );
    assert!(edges_of(&s, "opening", door) > 0.0);
    assert_eq!(edges_of(&s, "opening", window), 0.0);
    assert!(edges_of(&n, "opening", window) > 0.0);
    assert_eq!(edges_of(&n, "opening", door), 0.0);
    // Axis "A" (x = 0) crosses the south elevation, "1" (y = 0) is parallel to it.
    assert_eq!(s.count(|p| matches!(p, ViewPrim::Grid { .. })), 1);
    // Undo removes all four in one operation.
    m.undo().unwrap();
    assert!(m.view_defs().unwrap().is_empty());
}

#[test]
fn view_def_persist_update_delete_undo() {
    let p = std::env::temp_dir().join(format!("ddc_t64_{}.ddc", std::process::id()));
    let _ = std::fs::remove_file(&p);
    let path = p.to_str().unwrap();
    let id = {
        let mut m = Model::open(path).unwrap();
        assert_eq!(m.schema_version().unwrap(), crate::schema::VERSION);
        let id = m.add_view_def(&cut_spec()).unwrap();
        let mut s2 = cut_spec();
        s2.name = "2-2".into();
        s2.far_clip = true;
        s2.depth = 2.5;
        m.update_view_def(id, &s2).unwrap();
        id
    };
    let mut m = Model::open(path).unwrap();
    let s = m.view_def(id).unwrap();
    assert_eq!((s.name.as_str(), s.far_clip, s.depth), ("2-2", true, 2.5));
    m.undo().unwrap();
    assert_eq!(m.view_def(id).unwrap(), cut_spec());
    m.delete_view_def(id).unwrap();
    assert!(m.view_def(id).is_err());
    m.undo().unwrap();
    assert_eq!(m.view_def(id).unwrap(), cut_spec());
    let mut bad = cut_spec();
    bad.b = bad.a;
    assert!(m.add_view_def(&bad).is_err());
    drop(m);
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{path}{ext}"));
    }
}

#[test]
fn far_clip_hides_beyond() {
    let (m, _) = house();
    let mut s = cut_spec();
    let all = m.section_view(&s, &[]).unwrap();
    s.far_clip = true;
    s.depth = 0.1;
    let clipped = m.section_view(&s, &[]).unwrap();
    let n = |v: &PlanView| v.count(|p| matches!(p, ViewPrim::Edge { .. }));
    assert!(n(&clipped) < n(&all), "{} vs {}", n(&clipped), n(&all));
    assert!((cuts_area(&clipped) - 2.55325).abs() < 1e-9);
}

fn gable_model() -> (Model, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let mu = MullionSection::Rect { w: 0.2, d: 0.01 };
    let cw = m
        .add_curtain_wall(&CurtainSpec {
            level: l1,
            name: "G".into(),
            a: (0.0, 2.0),
            b: (4.0, 2.0),
            bulge: 0.0,
            base_offset: 0.0,
            height: 3.0,
            u: GridLines::Spacing(4.0),
            v: GridLines::Spacing(3.0),
            interior: mu,
            border: mu,
            lines: Default::default(),
            top: vec![(0.0, 0.1), (1.0, 0.1), (3.0, 3.0)],
        })
        .unwrap();
    (m, cw)
}

/// A panel trimmed by a polyline top with a vertex on the bottom of the outline keeps its polygon.
#[test]
fn gable_panel_keeps_true_outline() {
    let (m, cw) = gable_model();
    let l = m.curtain_layout(cw).unwrap();
    let cell = &l.layout.cells[0];
    let hmin = cell
        .outline
        .iter()
        .map(|q| q.1)
        .fold(f64::INFINITY, f64::min);
    assert!(
        cell.outline.iter().filter(|q| q.1 <= hmin + 1e-12).count() > 2,
        "{:?}",
        cell.outline
    );
    // Section along an axis through the panel: the glass area equals the panel's net area.
    let sec = SectionSpec {
        kind: ViewKind::Section,
        name: "s".into(),
        a: (-1.0, 2.0),
        b: (5.0, 2.0),
        depth: 5.0,
        far_clip: false,
    };
    let v = m.section_view(&sec, &[]).unwrap();
    let glass: f64 = {
        v.prims
            .iter()
            .filter_map(|p| match p {
                ViewPrim::SectionCut {
                    table: "curtain_wall",
                    element,
                    material,
                    rings,
                    ..
                } if *element == cw && material == "Glass" => Some(area(rings).abs()),
                _ => None,
            })
            .sum()
    };
    assert!(
        (glass - cell.net_area).abs() < 1e-9,
        "{glass} vs {}",
        cell.net_area
    );
    // Elevation: every vertex of the panel outline is an endpoint of a visible edge.
    let el = SectionSpec {
        kind: ViewKind::Elevation,
        name: "e".into(),
        a: (-1.0, -5.0),
        b: (5.0, -5.0),
        depth: 20.0,
        far_clip: false,
    };
    let v = m.section_view(&el, &[]).unwrap();
    let ends: Vec<(f64, f64)> = v
        .prims
        .iter()
        .filter_map(|p| match p {
            ViewPrim::Edge { a, b, .. } => Some([*a, *b]),
            _ => None,
        })
        .flatten()
        .collect();
    // Vertices on the bottom line lie on the continuous bottom edge (no endpoints), except the start of the slope.
    let slope_start = cell
        .outline
        .iter()
        .skip(2)
        .find(|q| q.1 <= hmin + 1e-9)
        .unwrap();
    for q in cell
        .outline
        .iter()
        .filter(|q| q.1 > hmin + 1e-9 || *q == slope_start)
    {
        let (s, z) = (q.0 + 1.0, l.z0 + q.1);
        assert!(
            ends.iter()
                .any(|e| (e.0 - s).abs() < 1e-6 && (e.1 - z).abs() < 1e-6),
            "vertex {q:?} missing"
        );
    }
}

/// Thick and arc walls entirely inside the elevation extent: the view plane does not cut them.
#[test]
fn compass_bounds_cover_full_footprint() {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let wt = m.add_wall_type("T", &layer("Brick", 0.4)).unwrap();
    let c = [(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)];
    let mut specs: Vec<WallSpec> = (0..4)
        .map(|k| WallSpec {
            a: c[k],
            b: c[(k + 1) % 4],
            level: l1,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        })
        .collect();
    m.add_walls(&specs).unwrap();
    specs.clear();
    for (x, y) in [(1.0, -3.0), (1.0, 7.0), (7.5, 0.0), (-3.5, 0.0)] {
        for (dx, dy, bulge) in [(0.0, 0.0, 0.4), (3.0, 0.0, -0.4)] {
            let (a, b) = if x > 7.0 || x < -3.0 {
                ((x, y + dx), (x, y + dx + 2.0))
            } else {
                ((x + dx, y + dy), (x + dx + 2.0, y + dy))
            };
            m.add_arc_wall(
                &WallSpec {
                    a,
                    b,
                    level: l1,
                    base_offset: 0.0,
                    height: 3.0,
                    wall_type: wt,
                    offset0: None,
                },
                bulge,
            )
            .unwrap();
        }
    }
    m.add_compass_elevations(0.0).unwrap();
    let sc = m.scene().unwrap();
    for (_, s) in m.view_defs().unwrap() {
        let (a, b) = (v2(s.a.0, s.a.1), v2(s.b.0, s.b.1));
        let fr = Frame {
            o: a,
            u: (b - a) * (1.0 / (b - a).len()),
            far: None,
            cut: false,
        };
        let out = section::section(&sc.prisms, &fr);
        assert!(
            out.cuts.is_empty(),
            "{}: {} cut regions",
            s.name,
            out.cuts.len()
        );
    }
}

/// A semicircle of radius 3 (axis), wall 0.2, a door along the arc between angles `d0`..`d1` degrees.
fn arc_door_model(d0: f64, d1: f64) -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let t = m.add_wall_type("W", &layer("Brick", 0.2)).unwrap();
    let w = m
        .add_arc_wall(
            &WallSpec {
                a: (3.0, 0.0),
                b: (-3.0, 0.0),
                level: l,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            },
            1.0,
        )
        .unwrap();
    m.add_opening(
        w,
        &OpeningSpec {
            along: 3.0 * d0.to_radians(),
            width: 3.0 * (d1 - d0).to_radians(),
            sill: 0.0,
            height: 2.1,
            kind: "door".into(),
        },
    )
    .unwrap();
    m
}

#[test]
fn door_in_arc_wall_is_cut_in_section() {
    let spec = SectionSpec {
        kind: ViewKind::Section,
        name: "A".into(),
        a: (0.0, -1.0),
        b: (0.0, 5.0),
        depth: 10.0,
        far_clip: false,
    };
    let total = |m: &Model| -> f64 {
        cuts(&m.section_view(&spec, &[]).unwrap())
            .iter()
            .map(|c| c.2.abs())
            .sum()
    };
    // Section along the y axis through the opening (80..100°): the wall above the door remains, 0.2 × 0.9.
    let through = total(&arc_door_model(80.0, 100.0));
    assert!((through - 0.18).abs() < 2e-3, "{through}");
    // Opening off to the side (10..30°): full section 0.2 × 3.
    let beside = total(&arc_door_model(10.0, 30.0));
    assert!((beside - 0.6).abs() < 2e-3, "{beside}");
}
