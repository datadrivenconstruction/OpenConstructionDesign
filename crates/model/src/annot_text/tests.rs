// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use crate::views::tests::room_6x4;
use crate::{LayerSpec, SectionSpec, SiteProps, SlabSpec, ViewKind, WallSpec};

fn width(s: &str, h: f64) -> f64 {
    s.chars().count() as f64 * h * 0.6
}

fn strokes(a: &Anno, scale: f64) -> Vec<AnnoStroke> {
    a.strokes(&AnnoCtx {
        scale,
        sep: ".",
        north: "N",
        east: "E",
        measure: &width,
    })
}

fn lines(s: &[AnnoStroke]) -> usize {
    s.iter()
        .filter(|x| matches!(x, AnnoStroke::Line { .. }))
        .count()
}

fn texts(s: &[AnnoStroke]) -> Vec<String> {
    s.iter()
        .filter_map(|x| match x {
            AnnoStroke::Text { s, .. } => Some(s.clone()),
            _ => None,
        })
        .collect()
}

fn style_id(m: &Model, name: &str) -> i64 {
    m.text_styles()
        .unwrap()
        .into_iter()
        .find(|s| s.name == name)
        .unwrap()
        .id
}

fn square(side: f64) -> Vec<P> {
    vec![(0.0, 0.0), (side, 0.0), (side, side), (0.0, side)]
}

#[test]
fn migration_is_repeatable_and_seeds_styles_once() {
    let (m, _) = room_6x4(&[("Brick", 0.2)]);
    assert!(crate::schema::STEPS.contains(&34));
    let n = m.text_styles().unwrap().len();
    assert!(n >= 5);
    assert!(m.line_styles().unwrap().iter().any(|s| !s.dash.is_empty()));
    assert!(m.detail_defs().unwrap().len() >= 8);
    migrate(&m.db).unwrap();
    assert_eq!(m.text_styles().unwrap().len(), n);
    let found: i64 =
        m.db.query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(found, crate::schema::VERSION);
}

#[test]
fn placement_is_one_undo_step_and_stays_in_its_view() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let other = m.add_level("L2", 3.0).unwrap();
    let mut note = TextNote::new((1.0, 2.0), "first\nsecond");
    note.leaders.push(Leader {
        to: (3.0, 1.0),
        elbow: Some((2.5, 1.5)),
    });
    let id = m
        .add_anno(
            AnnoView::Plan(lv),
            &AnnoShape::Text(note.clone()),
            None,
            None,
        )
        .unwrap();
    assert_eq!(m.anno(id).unwrap().shape, AnnoShape::Text(note));
    assert!(m.annos(AnnoView::Plan(other)).unwrap().is_empty());
    m.undo().unwrap();
    assert!(m.annos(AnnoView::Plan(lv)).unwrap().is_empty());
    m.assert_integrity();
}

#[test]
fn bad_shapes_and_missing_views_are_rejected() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let view = AnnoView::Plan(lv);
    let empty = AnnoShape::Text(TextNote::new((0.0, 0.0), "  "));
    assert!(m.add_anno(view, &empty, None, None).is_err());
    let line = AnnoShape::Line {
        pts: vec![(f64::NAN, 0.0), (1.0, 1.0)],
        closed: false,
    };
    assert!(m.add_anno(view, &line, None, None).is_err());
    let open = AnnoShape::Region {
        rings: vec![vec![(0.0, 0.0), (1.0, 0.0)]],
        fill: RegionFill::Mask,
        boundary: false,
    };
    assert!(m.add_anno(view, &open, None, None).is_err());
    let ok = AnnoShape::Text(TextNote::new((0.0, 0.0), "x"));
    assert!(m.add_anno(AnnoView::Plan(9999), &ok, None, None).is_err());
}

#[test]
fn edits_move_and_delete_are_undoable() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let view = AnnoView::Plan(lv);
    let shape = AnnoShape::Line {
        pts: vec![(0.0, 0.0), (1.0, 0.0)],
        closed: false,
    };
    let a = m.add_anno(view, &shape, None, None).unwrap();
    let b = m.add_anno(view, &shape, None, None).unwrap();
    m.move_annos(&[a, b], (1.0, 2.0)).unwrap();
    assert_eq!(m.anno(a).unwrap().shape.grips()[0], (1.0, 2.0));
    assert_eq!(m.anno(b).unwrap().shape.grips()[1], (2.0, 2.0));
    m.undo().unwrap();
    assert_eq!(m.anno(b).unwrap().shape, shape);
    m.update_anno(a, &shape.with_grip(1, (5.0, 5.0))).unwrap();
    assert_eq!(m.anno(a).unwrap().shape.grips()[1], (5.0, 5.0));
    m.undo().unwrap();
    m.delete_annos(&[a, b]).unwrap();
    assert!(m.annos(view).unwrap().is_empty());
    m.undo().unwrap();
    assert_eq!(m.annos(view).unwrap().len(), 2);
    let s = style_id(&m, "text_5");
    m.set_anno_styles(a, Some(s), None).unwrap();
    assert_eq!(m.anno(a).unwrap().text.height_mm, 5.0);
    m.undo().unwrap();
    assert_eq!(m.anno(a).unwrap().text_style, None);
}

#[test]
fn deleting_a_view_takes_its_annotations_and_undo_restores_them() {
    let (mut m, _) = room_6x4(&[("Brick", 0.2)]);
    let lv = m.add_level("L2", 3.0).unwrap();
    let sec = m
        .add_view_def(&SectionSpec {
            kind: ViewKind::Section,
            name: "1-1".into(),
            a: (3.0, -1.0),
            b: (3.0, 5.0),
            depth: 10.0,
            far_clip: false,
        })
        .unwrap();
    let note = AnnoShape::Text(TextNote::new((1.0, 1.0), "x"));
    m.add_anno(AnnoView::Plan(lv), &note, None, None).unwrap();
    m.add_anno(AnnoView::Section(sec), &note, None, None)
        .unwrap();
    m.set_anno_scale(AnnoView::Plan(lv), 50.0).unwrap();
    m.delete_level(lv).unwrap();
    assert!(m.annos(AnnoView::Plan(lv)).unwrap().is_empty());
    m.undo().unwrap();
    assert_eq!(m.annos(AnnoView::Plan(lv)).unwrap().len(), 1);
    assert_eq!(m.anno_scale(AnnoView::Plan(lv)).unwrap(), 50.0);
    m.delete_view_def(sec).unwrap();
    assert!(m.annos(AnnoView::Section(sec)).unwrap().is_empty());
    m.undo().unwrap();
    assert_eq!(m.annos(AnnoView::Section(sec)).unwrap().len(), 1);
    m.assert_integrity();
}

#[test]
fn view_drawings_carry_annotations() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let sec = m
        .add_view_def(&SectionSpec {
            kind: ViewKind::Section,
            name: "1-1".into(),
            a: (3.0, -1.0),
            b: (3.0, 5.0),
            depth: 10.0,
            far_clip: false,
        })
        .unwrap();
    let note = AnnoShape::Text(TextNote::new((1.0, 1.0), "joint"));
    m.add_anno(AnnoView::Plan(lv), &note, None, None).unwrap();
    m.add_anno(AnnoView::Section(sec), &note, None, None)
        .unwrap();
    let is = |p: &crate::ViewPrim| matches!(p, crate::ViewPrim::Anno { .. });
    let plan = m
        .plan_view(&crate::PlanViewSpec {
            level: lv,
            cut_height: 1.2,
            grids: vec![],
        })
        .unwrap();
    assert_eq!(plan.count(is), 1);
    assert_eq!(m.draw_view_def(sec, &[]).unwrap().count(is), 1);
}

#[test]
fn scale_comes_from_own_setting_then_viewport_then_default() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let view = AnnoView::Plan(lv);
    assert_eq!(m.anno_scale(view).unwrap(), 100.0);
    let sheet = m.add_sheet(&crate::SheetDef::new("A-1", "Plan")).unwrap();
    m.add_viewport(&crate::ViewportDef {
        sheet,
        view: crate::ViewRef::Plan {
            level: lv,
            cut: 1.2,
        },
        scale: 200.0,
        center: (100.0, 100.0),
        crop: None,
        title: "P".into(),
    })
    .unwrap();
    assert_eq!(m.anno_scale(view).unwrap(), 200.0);
    m.set_anno_scale(view, 50.0).unwrap();
    assert_eq!(m.anno_scale(view).unwrap(), 50.0);
    m.undo().unwrap();
    assert_eq!(m.anno_scale(view).unwrap(), 200.0);
    assert!(m.set_anno_scale(view, 0.0).is_err());
}

#[test]
fn text_keeps_paper_size_and_grows_in_model_units_with_scale() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let s = style_id(&m, "note_frame");
    let id = m
        .add_anno(
            AnnoView::Plan(lv),
            &AnnoShape::Text(TextNote::new((0.0, 0.0), crate::test_ru::ru("abc_upper"))),
            Some(s),
            None,
        )
        .unwrap();
    let a = m.anno(id).unwrap();
    let frame_w = |sc: f64| {
        strokes(&a, sc)
            .iter()
            .find_map(|x| match x {
                AnnoStroke::Line {
                    pts, closed: true, ..
                } => Some(pts[1].0 - pts[0].0),
                _ => None,
            })
            .unwrap()
    };
    let h = |sc: f64| {
        strokes(&a, sc)
            .iter()
            .find_map(|x| match x {
                AnnoStroke::Text { h_mm, .. } => Some(*h_mm),
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(h(50.0), h(200.0));
    assert!((frame_w(200.0) / frame_w(50.0) - 4.0).abs() < 1e-9);
}

#[test]
fn multiline_text_wraps_and_aligns() {
    let mut t = TextNote::new((0.0, 0.0), crate::test_ru::ru("wrap_in"));
    t.wrap_mm = Some(11.0);
    let a = Anno {
        id: 1,
        view: AnnoView::Plan(1),
        shape: AnnoShape::Text(t.clone()),
        text_style: None,
        line_style: None,
        text: TextStyle::default(),
        line: LineStyle::default(),
        detail: None,
        spot: None,
    };
    assert_eq!(
        texts(&strokes(&a, 100.0)),
        ["wrap_l1", "wrap_l2", "wrap_l3"].map(|k| crate::test_ru::ru(k).to_string())
    );
    t.wrap_mm = None;
    t.align = TextAlign::Right;
    let b = Anno {
        shape: AnnoShape::Text(t),
        ..a
    };
    let ss = strokes(&b, 100.0);
    let xs: Vec<f64> = ss
        .iter()
        .filter_map(|x| match x {
            AnnoStroke::Text { pos, s, h_mm, .. } => Some(pos.0 + width(s, *h_mm) * 0.1),
            _ => None,
        })
        .collect();
    assert_eq!(xs.len(), 2);
    assert!(xs.iter().all(|x| x.abs() < 1e-9), "{xs:?}");
}

#[test]
fn leader_reaches_target_with_a_head() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let mut t = TextNote::new((0.0, 0.0), "x");
    t.leaders.push(Leader {
        to: (5.0, -3.0),
        elbow: None,
    });
    let id = m
        .add_anno(AnnoView::Plan(lv), &AnnoShape::Text(t), None, None)
        .unwrap();
    let ss = strokes(&m.anno(id).unwrap(), 100.0);
    assert!(ss.iter().any(|x| matches!(
        x,
        AnnoStroke::Line { pts, .. } if pts.last() == Some(&(5.0, -3.0))
    )));
    assert!(ss.iter().any(|x| matches!(x, AnnoStroke::Fill { .. })));
}

#[test]
fn hatch_density_follows_scale_and_skips_holes() {
    let a = |rings: Vec<Vec<P>>| Anno {
        id: 1,
        view: AnnoView::Plan(1),
        shape: AnnoShape::Region {
            rings,
            fill: RegionFill::Hatch {
                deg: 0.0,
                step_mm: 1.0,
                cross: false,
            },
            boundary: false,
        },
        text_style: None,
        line_style: None,
        text: TextStyle::default(),
        line: LineStyle::default(),
        detail: None,
        spot: None,
    };
    let solid = a(vec![square(1.0)]);
    let (n100, n50) = (
        lines(&strokes(&solid, 100.0)),
        lines(&strokes(&solid, 50.0)),
    );
    assert!((9..=11).contains(&n100), "{n100}");
    assert!((19..=21).contains(&n50), "{n50}");
    // a full-height hole splits every line in two
    let hole = vec![(0.4, 0.0), (0.6, 0.0), (0.6, 1.0), (0.4, 1.0)];
    let holed = a(vec![square(1.0), hole]);
    let n = lines(&strokes(&holed, 100.0));
    assert_eq!(n, 2 * n100);
}

#[test]
fn hatch_line_count_is_capped() {
    let segs = layout_hatch_probe();
    assert!(segs <= 4001, "{segs}");
}

fn layout_hatch_probe() -> usize {
    let a = Anno {
        id: 1,
        view: AnnoView::Plan(1),
        shape: AnnoShape::Region {
            rings: vec![square(1000.0)],
            fill: RegionFill::Hatch {
                deg: 0.0,
                step_mm: 0.01,
                cross: false,
            },
            boundary: false,
        },
        text_style: None,
        line_style: None,
        text: TextStyle::default(),
        line: LineStyle::default(),
        detail: None,
        spot: None,
    };
    lines(&strokes(&a, 1.0))
}

#[test]
fn spot_elevation_reads_slab_and_level_at_each_draw() {
    let (mut m, _) = room_6x4(&[("Brick", 0.2)]);
    let lv = m.add_level("L2", 3.0).unwrap();
    let st = m
        .add_slab_type(
            "S",
            &[LayerSpec {
                material: "Concrete".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    m.add_slab(&SlabSpec {
        level: lv,
        slab_type: st,
        offset: 0.5,
        outer: square(4.0),
        holes: vec![],
    })
    .unwrap();
    let spot = |at: P| AnnoShape::Spot {
        kind: SpotKind::Elevation,
        at,
        head: (at.0 + 1.0, at.1 + 1.0),
    };
    let on = m
        .add_anno(AnnoView::Plan(lv), &spot((1.0, 1.0)), None, None)
        .unwrap();
    let off = m
        .add_anno(AnnoView::Plan(lv), &spot((9.0, 9.0)), None, None)
        .unwrap();
    assert_eq!(texts(&strokes(&m.anno(on).unwrap(), 100.0)), ["+3.500"]);
    // outside the slab the label is flagged honestly: it is a level elevation, not a surface
    assert_eq!(texts(&strokes(&m.anno(off).unwrap(), 100.0)), ["~+3.000"]);
}

fn coord(m: &mut Model, lv: i64, at: P) -> Vec<String> {
    let s = AnnoShape::Spot {
        kind: SpotKind::Coordinate,
        at,
        head: (at.0 + 1.0, at.1 + 1.0),
    };
    let id = m.add_anno(AnnoView::Plan(lv), &s, None, None).unwrap();
    texts(&strokes(&m.anno(id).unwrap(), 100.0))
}

fn set_north(m: &mut Model, north_deg: f64) {
    m.set_site(&SiteProps {
        name: "S".into(),
        north_deg,
        base: (100.0, 200.0, 0.0),
        latitude: 0.0,
        longitude: 0.0,
        ..Default::default()
    })
    .unwrap();
}

#[test]
fn spot_coordinates_follow_north_with_correct_sign() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    set_north(&mut m, 90.0);
    // project north = (−sin, cos) = −X: a point 10 m west along X is 10 m north of the base
    assert_eq!(coord(&mut m, lv, (-10.0, 0.0)), ["N 210.000", "E 100.000"]);
}

#[test]
fn spot_coordinates_invert_the_site_shift() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let north = 30.0f64;
    set_north(&mut m, north);
    // survey point to project: project = R(north)·(survey − base), as when anchoring terrain
    let (x, y) = (130.0 - 100.0, 250.0 - 200.0);
    let (s, c) = north.to_radians().sin_cos();
    let at = (x * c - y * s, x * s + y * c);
    assert_eq!(coord(&mut m, lv, at), ["N 250.000", "E 130.000"]);
}

fn concrete(m: &mut Model) -> i64 {
    m.add_slab_type(
        "S",
        &[LayerSpec {
            material: "Concrete".into(),
            thickness: 0.2,
        }],
    )
    .unwrap()
}

#[test]
fn spot_elevation_reads_roof_and_skips_shaft_cut() {
    let (mut m, _) = room_6x4(&[("Brick", 0.2)]);
    let lv = m.add_level("L2", 3.0).unwrap();
    let st = concrete(&mut m);
    m.add_slab(&SlabSpec {
        level: lv,
        slab_type: st,
        offset: 0.5,
        outer: vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)],
        holes: vec![],
    })
    .unwrap();
    let spot = |at: P| AnnoShape::Spot {
        kind: SpotKind::Elevation,
        at,
        head: (at.0 + 1.0, at.1 + 1.0),
    };
    let on_slab = m
        .add_anno(AnnoView::Plan(lv), &spot((1.0, 1.0)), None, None)
        .unwrap();
    assert_eq!(
        texts(&strokes(&m.anno(on_slab).unwrap(), 100.0)),
        ["+3.500"]
    );
    m.add_shaft(&crate::ShaftSpec {
        level: lv,
        top_level: lv,
        base_offset: 0.0,
        top_offset: 1.0,
        ring: vec![(0.5, 0.5), (1.5, 0.5), (1.5, 1.5), (0.5, 1.5)],
    })
    .unwrap();
    // a shaft cut the slab at this point: there is no surface
    assert_eq!(
        texts(&strokes(&m.anno(on_slab).unwrap(), 100.0)),
        ["~+3.000"]
    );
    m.add_roof(&crate::RoofSpec {
        level: lv,
        slab_type: st,
        offset: 3.0,
        overhang: 0.0,
        fascia: 0.2,
        kind: crate::RoofKind::Footprint {
            ring: vec![(20.0, 0.0), (30.0, 0.0), (30.0, 10.0), (20.0, 10.0)],
            slopes: vec![Some(30.0); 4],
            mansard: None,
        },
    })
    .unwrap();
    let on_roof = m
        .add_anno(AnnoView::Plan(lv), &spot((22.0, 5.0)), None, None)
        .unwrap();
    let t = texts(&strokes(&m.anno(on_roof).unwrap(), 100.0));
    let z: f64 = t[0].trim_start_matches('+').parse().unwrap();
    assert!(z > 6.0, "above the eave: {t:?}");
}

#[test]
fn section_spot_snaps_to_surface_and_marks_when_it_leaves() {
    let (mut m, _) = room_6x4(&[("Brick", 0.2)]);
    let lv = m.add_level("L2", 3.0).unwrap();
    let st = concrete(&mut m);
    let slab = m
        .add_slab(&SlabSpec {
            level: lv,
            slab_type: st,
            offset: 0.0,
            outer: vec![(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)],
            holes: vec![],
        })
        .unwrap();
    let sec = m
        .add_view_def(&SectionSpec {
            kind: ViewKind::Section,
            name: "1-1".into(),
            a: (0.0, 1.0),
            b: (6.0, 1.0),
            depth: 10.0,
            far_clip: false,
        })
        .unwrap();
    let e = m
        .add_anno(
            AnnoView::Section(sec),
            &AnnoShape::Spot {
                kind: SpotKind::Elevation,
                at: (2.0, 3.1),
                head: (3.0, 4.0),
            },
            None,
            None,
        )
        .unwrap();
    // a click 0.1 m higher: the label still comes from the surface
    let read = |m: &Model| texts(&strokes(&m.anno(e).unwrap(), 100.0));
    assert_eq!(read(&m), ["+3.000"]);
    m.delete_slab(slab).unwrap();
    assert_eq!(read(&m), ["~+3.100"]);
}

#[test]
fn bad_row_is_skipped_and_reported() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let view = AnnoView::Plan(lv);
    let good = m
        .add_anno(
            view,
            &AnnoShape::Text(TextNote::new((0.0, 0.0), "a")),
            None,
            None,
        )
        .unwrap();
    let bad = m
        .add_anno(
            view,
            &AnnoShape::Text(TextNote::new((1.0, 0.0), "b")),
            None,
            None,
        )
        .unwrap();
    m.db.execute(
        "UPDATE anno SET data='{\"pos\":[1,0],\"rot\":null}' WHERE id=?1",
        [bad],
    )
    .unwrap();
    let ids: Vec<i64> = m.annos(view).unwrap().iter().map(|a| a.id).collect();
    assert_eq!(ids, [good]);
    let issues = m.check_integrity().unwrap();
    assert!(issues.iter().any(|i| i.table == "anno" && i.id == bad));
}

#[test]
fn non_finite_values_are_refused_on_write() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let mut t = TextNote::new((0.0, 0.0), "a");
    t.rot_deg = f64::NAN;
    assert!(m
        .add_anno(AnnoView::Plan(lv), &AnnoShape::Text(t), None, None)
        .is_err());
}

#[test]
fn transforms_rotate_mirror_and_copy_as_one_step() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let view = AnnoView::Plan(lv);
    let line = AnnoShape::Line {
        pts: vec![(1.0, 0.0), (2.0, 0.0)],
        closed: false,
    };
    let style = m.line_styles().unwrap()[1].id;
    let id = m.add_anno(view, &line, None, Some(style)).unwrap();
    let close = |a: P, b: P| (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9;
    let quarter = AnnoXf::Rotate {
        o: (0.0, 0.0),
        a: std::f64::consts::FRAC_PI_2,
    };
    m.transform_annos(&[id], quarter).unwrap();
    let g = m.anno(id).unwrap().shape.grips();
    assert!(close(g[0], (0.0, 1.0)) && close(g[1], (0.0, 2.0)));
    m.undo().unwrap();
    assert_eq!(m.anno(id).unwrap().shape, line);
    // a degenerate reflection axis does not corrupt the row
    let degenerate = AnnoXf::Mirror {
        a: (1.0, 1.0),
        b: (1.0, 1.0),
    };
    assert!(m.transform_annos(&[id], degenerate).is_err());
    assert_eq!(m.anno(id).unwrap().shape, line);
    let xfs: Vec<AnnoXf> = (1..3i32)
        .map(|k| AnnoXf::Move((0.0, f64::from(k))))
        .collect();
    let copies = m.copy_annos(&[id], &xfs).unwrap();
    assert_eq!(copies.len(), 2);
    let c = m.anno(copies[1]).unwrap();
    assert_eq!(c.view, view);
    assert_eq!(c.line_style, Some(style));
    assert!(close(c.shape.grips()[0], (1.0, 2.0)));
    m.undo().unwrap();
    assert_eq!(m.annos(view).unwrap().len(), 1);
    assert!(m.copy_annos(&[id], &[]).is_err());
    m.move_annos(&[id], (1.0, 0.0)).unwrap();
    assert!(close(m.anno(id).unwrap().shape.grips()[0], (2.0, 0.0)));
}

#[test]
fn mirror_keeps_text_readable_and_flips_details() {
    // reflection across the vertical x=0
    let x = AnnoXf::Mirror {
        a: (0.0, 0.0),
        b: (0.0, 1.0),
    };
    let t = AnnoShape::Text(TextNote::new((2.0, 1.0), "a")).transformed(x);
    let AnnoShape::Text(t) = t else { panic!() };
    assert!(t.rot_deg.abs() < 1e-9, "not upside down: {}", t.rot_deg);
    assert_eq!(t.align, TextAlign::Right);
    assert!((t.pos.0 + 2.0).abs() < 1e-9 && (t.pos.1 - 1.0).abs() < 1e-9);
    let d = AnnoShape::Detail {
        def: 1,
        at: (1.0, 0.0),
        rot_deg: 0.0,
        length: None,
        flip: false,
    }
    .transformed(x);
    let AnnoShape::Detail {
        at, rot_deg, flip, ..
    } = d
    else {
        panic!()
    };
    assert!(flip);
    // α = 90°: 2α − 0 = 180°, the component's X axis points to −X, as with a reflection
    assert!((rot_deg.abs() - 180.0).abs() < 1e-9);
    assert!((at.0 + 1.0).abs() < 1e-9);
}

#[test]
fn strokes_bbox_covers_rotated_text() {
    let a = (0.0, 0.0);
    let s = vec![AnnoStroke::Text {
        pos: a,
        h_mm: 2.5,
        rot_deg: 90.0,
        s: "abcd".into(),
    }];
    let c = AnnoCtx {
        scale: 100.0,
        sep: ".",
        north: "N",
        east: "E",
        measure: &width,
    };
    let bb = strokes_bbox(&s, &c).unwrap();
    // width 4·2.5·0.6 = 6 mm = 0.6 m along Y, height 0.25 m to the left
    assert!((bb[3] - 0.6).abs() < 1e-9 && (bb[0] + 0.25).abs() < 1e-9);
    assert!(strokes_bbox(&[], &c).is_none());
}

#[test]
fn spot_in_section_and_coordinates_use_view_and_site() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    m.set_site(&SiteProps {
        name: "S".into(),
        north_deg: 0.0,
        base: (100.0, 200.0, 0.0),
        latitude: 0.0,
        longitude: 0.0,
        ..Default::default()
    })
    .unwrap();
    let c = m
        .add_anno(
            AnnoView::Plan(lv),
            &AnnoShape::Spot {
                kind: SpotKind::Coordinate,
                at: (1.0, 2.0),
                head: (2.0, 3.0),
            },
            None,
            None,
        )
        .unwrap();
    assert_eq!(
        texts(&strokes(&m.anno(c).unwrap(), 100.0)),
        ["N 202.000", "E 101.000"]
    );
    let sec = m
        .add_view_def(&SectionSpec {
            kind: ViewKind::Section,
            name: "1-1".into(),
            a: (0.0, 1.0),
            b: (6.0, 1.0),
            depth: 10.0,
            far_clip: false,
        })
        .unwrap();
    let e = m
        .add_anno(
            AnnoView::Section(sec),
            &AnnoShape::Spot {
                kind: SpotKind::Elevation,
                at: (2.0, 2.75),
                head: (3.0, 3.5),
            },
            None,
            None,
        )
        .unwrap();
    assert_eq!(texts(&strokes(&m.anno(e).unwrap(), 100.0)), ["~+2.750"]);
    let sc = AnnoShape::Spot {
        kind: SpotKind::Coordinate,
        at: (2.0, 2.75),
        head: (3.0, 3.5),
    };
    let n = m.add_anno(AnnoView::Section(sec), &sc, None, None).unwrap();
    assert_eq!(
        texts(&strokes(&m.anno(n).unwrap(), 100.0)),
        ["N 201.000", "E 102.000"]
    );
}

#[test]
fn details_fit_fixed_stretch_and_repeat() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let defs = m.detail_defs().unwrap();
    let id = |n: &str| defs.iter().find(|d| d.name == n).unwrap().id;
    let view = AnnoView::Plan(lv);
    let place = |def, length| AnnoShape::Detail {
        def,
        at: (0.0, 0.0),
        rot_deg: 0.0,
        length,
        flip: false,
    };
    let beam = m
        .add_anno(view, &place(id("i_beam_200"), Some(9.0)), None, None)
        .unwrap();
    assert_eq!(lines(&strokes(&m.anno(beam).unwrap(), 50.0)), 1);
    let ins = m
        .add_anno(view, &place(id("insulation"), Some(1.0)), None, None)
        .unwrap();
    assert_eq!(lines(&strokes(&m.anno(ins).unwrap(), 50.0)), 10);
    let brk = m
        .add_anno(view, &place(id("break_line"), Some(4.0)), None, None)
        .unwrap();
    let ss = strokes(&m.anno(brk).unwrap(), 50.0);
    assert!(matches!(
        &ss[0],
        AnnoStroke::Line { pts, .. } if (pts.last().unwrap().0 - 4.0).abs() < 1e-9
    ));
    let own = m
        .add_detail_def(&DetailDef {
            id: 0,
            name: "own".into(),
            length: 1.0,
            fit: DetailFit::Fixed,
            strokes: vec![DetailStroke {
                pts: vec![(0.0, 0.0), (1.0, 0.0)],
                closed: false,
                fill: false,
            }],
        })
        .unwrap();
    assert!(m.detail_defs().unwrap().iter().any(|d| d.id == own));
    m.undo().unwrap();
    assert!(m.detail_defs().unwrap().iter().all(|d| d.id != own));
}

#[test]
fn cloud_bumps_go_outward_for_both_ring_directions() {
    let cloud = |ring: Vec<P>| Anno {
        id: 1,
        view: AnnoView::Plan(1),
        shape: AnnoShape::Cloud {
            ring,
            mark: "1".into(),
        },
        text_style: None,
        line_style: None,
        text: TextStyle::default(),
        line: LineStyle::default(),
        detail: None,
        spot: None,
    };
    let mut cw = square(10.0);
    cw.reverse();
    for ring in [square(10.0), cw] {
        let ss = strokes(&cloud(ring), 100.0);
        let AnnoStroke::Line { pts, closed, .. } = &ss[0] else {
            panic!("cloud outline expected");
        };
        assert!(*closed);
        assert!(pts.len() > 40);
        let lo = pts.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
        assert!(lo < -0.01, "{lo}");
        assert_eq!(texts(&ss), ["1"]);
    }
}

#[test]
fn hit_test_finds_lines_fills_and_text() {
    let ctx = AnnoCtx {
        scale: 100.0,
        sep: ".",
        north: "N",
        east: "E",
        measure: &width,
    };
    let ss = vec![
        AnnoStroke::Line {
            pts: vec![(0.0, 0.0), (1.0, 0.0)],
            closed: false,
            weight_mm: 0.2,
            dash_mm: vec![],
        },
        AnnoStroke::Fill {
            rings: vec![vec![(5.0, 5.0), (6.0, 5.0), (6.0, 6.0)]],
            gray: 0,
            mask: false,
        },
        AnnoStroke::Text {
            pos: (10.0, 10.0),
            h_mm: 2.5,
            rot_deg: 0.0,
            s: "abcd".into(),
        },
    ];
    assert!(stroke_hit(&ss, (0.5, 0.05), 0.1, &ctx));
    assert!(!stroke_hit(&ss, (0.5, 0.5), 0.1, &ctx));
    assert!(stroke_hit(&ss, (5.9, 5.5), 0.0, &ctx));
    assert!(stroke_hit(&ss, (10.3, 10.1), 0.0, &ctx));
    assert!(!stroke_hit(&ss, (12.0, 10.1), 0.0, &ctx));
}

#[test]
fn dash_pattern_splits_and_stops_at_the_end() {
    assert_eq!(dash_segments((0.0, 0.0), (1.0, 0.0), &[]).len(), 1);
    let v = dash_segments((0.0, 0.0), (10.0, 0.0), &[2.0, 1.0]);
    assert_eq!(v.len(), 4);
    assert_eq!(v[3].1, (10.0, 0.0));
    assert_eq!(
        dash_segments((0.0, 0.0), (1.0, 0.0), &[-1.0, -1.0]).len(),
        1
    );
}

#[test]
fn legend_rows_view_places_and_cascade() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let line = m.line_styles().unwrap()[0].id;
    let wt: i64 =
        m.db.query_row("SELECT id FROM wall_type LIMIT 1", [], |r| r.get(0))
            .unwrap();
    let id = m.add_legend("Legend", 50.0).unwrap();
    let rows = vec![
        LegendRow {
            swatch: LegendSwatch::Line(line),
            label: "Line".into(),
        },
        LegendRow {
            swatch: LegendSwatch::Region(RegionFill::Solid { gray: 128 }),
            label: "Fill".into(),
        },
        LegendRow {
            swatch: LegendSwatch::WallType(wt),
            label: "Wall".into(),
        },
    ];
    m.set_legend_rows(id, &rows).unwrap();
    assert_eq!(m.legend_rows(id).unwrap(), rows);
    m.undo().unwrap();
    assert!(m.legend_rows(id).unwrap().is_empty());
    m.set_legend_rows(id, &rows).unwrap();
    let v = m.legend_view(id).unwrap();
    assert!(v.prims.len() >= 7, "{}", v.prims.len());
    let sheet = m.add_sheet(&crate::SheetDef::new("A-1", "Sheet")).unwrap();
    let place = m.place_legend(sheet, id, (20.0, 200.0)).unwrap();
    assert_eq!(m.legend_places(sheet).unwrap()[0].id, place);
    m.move_legend_place(place, (30.0, 180.0)).unwrap();
    assert_eq!(m.legend_places(sheet).unwrap()[0].pos, (30.0, 180.0));
    m.delete_legend(id).unwrap();
    assert!(m.legends().unwrap().is_empty());
    assert!(m.legend_places(sheet).unwrap().is_empty());
    m.undo().unwrap();
    assert_eq!(m.legend_rows(id).unwrap(), rows);
    assert_eq!(m.legend_places(sheet).unwrap().len(), 1);
    m.delete_sheet(sheet).unwrap();
    assert!(m.legend_places(sheet).unwrap().is_empty());
    m.undo().unwrap();
    assert_eq!(m.legend_places(sheet).unwrap().len(), 1);
    m.assert_integrity();
    let rid = m
        .add_anno(
            AnnoView::Plan(lv),
            &AnnoShape::Region {
                rings: vec![square(1.0)],
                fill: RegionFill::Mask,
                boundary: false,
            },
            None,
            None,
        )
        .unwrap();
    assert!(rid > 0);
    let from_view = m.legend_rows_from_view(AnnoView::Plan(lv)).unwrap();
    assert_eq!(from_view[0].swatch, LegendSwatch::Region(RegionFill::Mask));
}

/// Level dimensions in mm, ascending.
fn dim_sizes(m: &Model, level: i64) -> Vec<i64> {
    let mut v: Vec<i64> = m
        .dims(level)
        .unwrap()
        .iter()
        .map(|d| (d.geom.value() * 1000.0).round() as i64)
        .collect();
    v.sort_unstable();
    v
}

fn two_collinear_walls() -> (Model, i64, Vec<i64>) {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let wt = m
        .add_wall_type(
            "W",
            &[LayerSpec {
                material: "Brick".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    let wall = |a, b| WallSpec {
        a,
        b,
        level: lv,
        base_offset: 0.0,
        height: 3.0,
        wall_type: wt,
        offset0: Some(0.0),
    };
    let ids = m
        .add_walls(&[
            wall((0.0, 0.0), (3.0, 0.0)),
            wall((3.0, 0.0), (7.0, 0.0)),
            wall((7.0, 0.0), (7.0, 5.0)),
        ])
        .unwrap();
    (m, lv, ids)
}

#[test]
fn chain_of_collinear_walls_gets_joint_and_overall_dimensions() {
    let (mut m, lv, ids) = two_collinear_walls();
    // one wall pulls in its collinear neighbour; a perpendicular one is not included
    let dims = m.auto_dimension_chain(lv, &ids[..1]).unwrap();
    assert_eq!(dims.len(), 3);
    assert_eq!(dim_sizes(&m, lv), [3000, 4000, 7000]);
    // a repeat replaces rather than duplicates; undo is one step
    m.auto_dimension_chain(lv, &ids[..1]).unwrap();
    assert_eq!(m.dims(lv).unwrap().len(), 3);
    m.undo().unwrap();
    assert_eq!(m.dims(lv).unwrap().len(), 3);
    m.undo().unwrap();
    assert!(m.dims(lv).unwrap().is_empty());
    m.assert_integrity();
}

#[test]
fn chain_with_several_walls_makes_one_run_per_line() {
    let (mut m, lv, ids) = two_collinear_walls();
    // the first two are one line, the third is its own (overall dimension only)
    let dims = m.auto_dimension_chain(lv, &ids).unwrap();
    assert_eq!(dims.len(), 4);
}

#[test]
fn collinear_walls_across_a_gap_are_separate_chains() {
    let (mut m, lv, ids) = two_collinear_walls();
    let wt: i64 =
        m.db.query_row("SELECT wtype FROM wall WHERE id=?1", [ids[0]], |r| r.get(0))
            .unwrap();
    let far = m
        .add_walls(&[WallSpec {
            level: lv,
            a: (12.0, 0.0),
            b: (15.0, 0.0),
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: Some(0.0),
        }])
        .unwrap()[0];
    // a single chain would give a dimension across the empty gap 7→12
    m.auto_dimension_chain(lv, &[ids[0], ids[1], far]).unwrap();
    let sizes = dim_sizes(&m, lv);
    assert!(!sizes.contains(&5000), "{sizes:?}");
    assert!(sizes.contains(&3000) && sizes.contains(&4000) && sizes.contains(&7000));
}

#[test]
fn chain_skipped_counts_only_walls_it_cannot_take() {
    let (m, _, ids) = two_collinear_walls();
    assert_eq!(m.chain_skipped(&ids).unwrap(), 0);
    assert_eq!(m.chain_skipped(&[ids[0], -1]).unwrap(), 1);
}

#[test]
fn chain_of_single_wall_matches_wall_dimensions() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let wall: i64 =
        m.db.query_row(
            "SELECT wall FROM opening WHERE kind='door' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let a = m.auto_dimension_walls(lv, &[wall]).unwrap();
    let before = dim_sizes(&m, lv);
    m.undo().unwrap();
    let b = m.auto_dimension_chain(lv, &[wall]).unwrap();
    assert_eq!(a.len(), b.len());
    assert_eq!(before, dim_sizes(&m, lv));
}

#[test]
fn generic_edit_tools_take_annotations_with_one_undo() {
    use crate::{Spacing, Target};
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let view = AnnoView::Plan(lv);
    let line = AnnoShape::Line {
        pts: vec![(1.0, 0.0), (2.0, 0.0)],
        closed: false,
    };
    let id = m.add_anno(view, &line, None, None).unwrap();
    let close = |a: P, b: P| (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9;
    let t = [Target::Anno(id)];

    m.move_elements(&t, (0.5, 0.25)).unwrap();
    let g = m.anno(id).unwrap().shape.grips();
    assert!(close(g[0], (1.5, 0.25)) && close(g[1], (2.5, 0.25)));
    m.undo().unwrap();
    assert_eq!(m.anno(id).unwrap().shape, line);

    m.rotate_elements(&t, (1.0, 0.0), std::f64::consts::FRAC_PI_2, false)
        .unwrap();
    let g = m.anno(id).unwrap().shape.grips();
    assert!(close(g[0], (1.0, 0.0)) && close(g[1], (1.0, 1.0)), "{g:?}");
    m.undo().unwrap();
    assert_eq!(m.anno(id).unwrap().shape, line);

    let copies = m.copy_elements(&t, &[(0.0, 3.0)]).unwrap();
    let &[Target::Anno(c)] = copies.as_slice() else {
        panic!("{copies:?}")
    };
    assert_eq!(m.anno(c).unwrap().view, view);
    assert!(close(m.anno(c).unwrap().shape.grips()[0], (1.0, 3.0)));
    m.undo().unwrap();
    assert_eq!(m.annos(view).unwrap().len(), 1);

    let arr = m.array_linear(&t, 3, Spacing::Step((0.0, 1.0))).unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(m.annos(view).unwrap().len(), 3);
    m.undo().unwrap();
    assert_eq!(m.annos(view).unwrap().len(), 1);
}

#[test]
fn generic_mirror_matches_the_annotation_mirror() {
    use crate::Target;
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let view = AnnoView::Plan(lv);
    let mut note = TextNote::new((1.0, 2.0), "A-1");
    note.rot_deg = 30.0;
    let text = AnnoShape::Text(note);
    let a = m.add_anno(view, &text, None, None).unwrap();
    let b = m.add_anno(view, &text, None, None).unwrap();
    let (p, q) = ((-1.0, 0.5), (2.0, 3.0));
    m.mirror_elements(&[Target::Anno(a)], p, q, false).unwrap();
    m.transform_annos(&[b], AnnoXf::Mirror { a: p, b: q }).unwrap();
    let (sa, sb) = (m.anno(a).unwrap().shape, m.anno(b).unwrap().shape);
    let (AnnoShape::Text(ta), AnnoShape::Text(tb)) = (&sa, &sb) else {
        panic!("not text")
    };
    assert!((ta.pos.0 - tb.pos.0).abs() < 1e-9 && (ta.pos.1 - tb.pos.1).abs() < 1e-9);
    assert!((ta.rot_deg - tb.rot_deg).abs() < 1e-9);
    assert_eq!(ta.align, tb.align);
    m.undo().unwrap();
    m.undo().unwrap();
    assert_eq!(m.anno(a).unwrap().shape, text);
}

fn seed_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("ddc_mig2_anno_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The open-time upgrade on a plain connection: how many rows it changed.
fn upgrade_writes(p: &str) -> u64 {
    let mut c = Connection::open(p).unwrap();
    crate::schema::upgrade(&mut c, Some(p)).unwrap();
    c.total_changes()
}

fn names(m: &Model) -> (Vec<String>, Vec<String>, Vec<String>) {
    (
        m.text_styles().unwrap().into_iter().map(|s| s.name).collect(),
        m.line_styles().unwrap().into_iter().map(|s| s.name).collect(),
        m.detail_defs().unwrap().into_iter().map(|d| d.name).collect(),
    )
}

/// Builtin styles and details the user removed (purge) stay removed after save and reopen;
/// reopening writes nothing, saves the same bytes and leaves nothing to undo.
#[test]
fn deleted_builtin_styles_stay_deleted_on_reopen() {
    let d = seed_dir("del");
    let (p, a, b) = (d.join("m.ddc"), d.join("a.ddc"), d.join("b.ddc"));
    let ps = p.to_str().unwrap();
    let detail = builtin_details()[0].name.clone();
    let before = {
        let m = Model::open(ps).unwrap();
        m.db.execute("DELETE FROM anno_text_style WHERE name='title_7'", [])
            .unwrap();
        m.db.execute("DELETE FROM anno_line_style WHERE name='hidden'", [])
            .unwrap();
        m.db.execute("DELETE FROM anno_detail_def WHERE name=?1", [&detail])
            .unwrap();
        names(&m)
    };
    assert!(!before.0.iter().any(|n| n == "title_7"));
    assert!(!before.1.iter().any(|n| n == "hidden"));
    assert!(!before.2.contains(&detail));
    assert_eq!(upgrade_writes(ps), 0, "first reopen");
    assert_eq!(upgrade_writes(ps), 0, "second reopen");
    {
        let m = Model::open(ps).unwrap();
        assert_eq!(names(&m), before, "deletion survives reopen");
        m.snapshot_to(&a).unwrap();
    }
    let mut m = Model::open(ps).unwrap();
    assert_eq!(names(&m), before);
    m.snapshot_to(&b).unwrap();
    assert!(std::fs::read(&a).unwrap() == std::fs::read(&b).unwrap(), "saves differ");
    assert!(matches!(m.undo(), Err(Error::NothingToUndo)));
    assert_eq!(names(&m), before, "undo after open brings nothing back");
    drop(m);
    let _ = std::fs::remove_dir_all(d);
}

/// An older file without the style tables gets the builtin set once, by the upgrade; a table
/// it already had keeps the user's deletion.
#[test]
fn an_old_file_is_seeded_once() {
    let d = seed_dir("old");
    let p = d.join("m.ddc");
    let ps = p.to_str().unwrap();
    drop(Model::open(ps).unwrap());
    {
        let c = Connection::open(ps).unwrap();
        c.execute_batch(
            "DROP TABLE anno_line_style; DROP TABLE anno_detail_def;
             DELETE FROM anno_text_style WHERE name='title_7'; DELETE FROM op;",
        )
        .unwrap();
        c.execute(
            "UPDATE meta SET value=?1 WHERE key='schema_version'",
            [crate::schema::VERSION - 1],
        )
        .unwrap();
    }
    let mut m = Model::open(ps).unwrap();
    let seeded = names(&m);
    assert_eq!(seeded.1.len(), LINE_SEED.len(), "line styles seeded by the upgrade");
    assert_eq!(seeded.2.len(), builtin_details().len(), "details seeded by the upgrade");
    assert!(!seeded.0.iter().any(|n| n == "title_7"), "kept table keeps the deletion");
    assert!(matches!(m.undo(), Err(Error::NothingToUndo)));
    drop(m);
    assert_eq!(upgrade_writes(ps), 0, "seeded once");
    let m = Model::open(ps).unwrap();
    assert_eq!(names(&m), seeded);
    drop(m);
    let _ = std::fs::remove_dir_all(d);
}
