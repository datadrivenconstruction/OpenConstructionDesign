// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::synth::{score, SynthOpening, SynthPlan, SynthWall, WallStyle};
use super::*;
use crate::drawing_model::Underlay;
use crate::pdf::export_pdf;
use crate::pdf::import::{ImportOptions, PdfFile, Scale};

/// Two rooms 4 x 6 m behind 300 mm walls, a 120 mm partition, two windows and two doors.
fn house() -> SynthPlan {
    let w = |a: (f64, f64), b: (f64, f64), t: f64| SynthWall { a, b, thickness: t };
    let o = |wall: usize, along: f64, width: f64, door: bool| SynthOpening { wall, along, width, door };
    SynthPlan {
        walls: vec![
            w((0.0, 0.0), (8.0, 0.0), 0.3),
            w((8.0, 0.0), (8.0, 6.0), 0.3),
            w((8.0, 6.0), (0.0, 6.0), 0.3),
            w((0.0, 6.0), (0.0, 0.0), 0.3),
            w((4.0, 0.0), (4.0, 6.0), 0.12),
        ],
        openings: vec![
            o(0, 1.0, 1.5, false),
            o(2, 1.5, 1.2, false),
            o(4, 2.0, 0.9, true),
            o(1, 2.5, 1.0, true),
        ],
    }
}

fn rotated(p: &SynthPlan, deg: f64) -> SynthPlan {
    let (c, s) = (deg.to_radians().cos(), deg.to_radians().sin());
    let r = |q: (f64, f64)| (q.0 * c - q.1 * s, q.0 * s + q.1 * c);
    SynthPlan {
        walls: p.walls.iter().map(|w| SynthWall { a: r(w.a), b: r(w.b), thickness: w.thickness }).collect(),
        openings: p.openings.clone(),
    }
}

fn read(p: &SynthPlan, k: f64, style: WallStyle, scale: Option<ImageScale>) -> (PlanRead, (f64, f64)) {
    let (img, shift) = p.render(k, 1.0, style);
    let r = read_plan(&img, &scale.unwrap_or(ImageScale::PxPerM(k)), &[], None).unwrap();
    (r, shift)
}

#[test]
fn a_filled_rectangle_keeps_its_outline_and_a_thin_line_its_centre() {
    let mut g = Gray::new(60, 40);
    for y in 10..20 {
        for x in 10..50 {
            g.px[y * 60 + x] = 0;
        }
    }
    for x in 5..55 {
        g.px[30 * 60 + x] = 0;
        g.px[31 * 60 + x] = 0;
    }
    let m = raster::masks(&g);
    assert!(m.thick[15 * 60 + 30]);
    assert!(!m.thick[30 * 60 + 30]);
    // the outline of the block, not its inside
    assert!(m.skel[10 * 60 + 30] && m.skel[19 * 60 + 30] && !m.skel[15 * 60 + 30]);
    // the two-pixel line is one pixel wide now
    assert_eq!((m.skel[30 * 60 + 30] as u8) + (m.skel[31 * 60 + 30] as u8), 1);
}

#[test]
fn straight_runs_are_found_with_their_ends_and_curves_are_left() {
    let (w, h) = (200, 120);
    let mut skel = vec![false; w * h];
    for x in 20..180 {
        skel[50 * w + x] = true;
    }
    // a quarter circle of radius 45 px, clear of the line
    for k in 0..400 {
        let a = std::f64::consts::FRAC_PI_2 * k as f64 / 400.0;
        let (x, y) = (100.0 + 45.0 * a.cos(), 110.0 - 45.0 * a.sin());
        skel[y as usize * w + x as usize] = true;
    }
    let segs = lines::segments(w, h, &mut skel, 30.0, true);
    assert_eq!(segs.len(), 1, "{segs:?}");
    let s = segs[0];
    let (x0, x1) = (s.a.0.min(s.b.0), s.a.0.max(s.b.0));
    assert!((x0 - 20.0).abs() < 1.0 && (x1 - 180.0).abs() < 1.0, "{s:?}");
    assert!((s.a.1 - 50.5).abs() < 0.2 && (s.b.1 - 50.5).abs() < 0.2, "{s:?}");
    let arcs = arcs::arcs(w, h, &skel, (30.0, 60.0));
    assert_eq!(arcs.len(), 1);
    assert!((arcs[0].r - 45.0).abs() < 1.5, "{:?}", arcs[0]);
}

/// At 50 px/m the long-line pass takes runs from 22.5 px: a chord that long of a 45 px swing
/// lies within the line band and its residual, but it bows, so the swing stays whole.
#[test]
fn a_short_chord_of_a_swing_is_not_a_straight_run() {
    let (w, h) = (120, 120);
    for r in [45.0, 50.0, 60.0] {
        let mut skel = vec![false; w * h];
        for k in 0..2000 {
            let a = std::f64::consts::FRAC_PI_2 * k as f64 / 2000.0;
            let (x, y) = (10.0 + r * a.cos(), 110.0 - r * a.sin());
            skel[y as usize * w + x as usize] = true;
        }
        let segs = lines::segments(w, h, &mut skel, 22.5, true);
        assert!(segs.is_empty(), "r {r}: {segs:?}");
        let found = arcs::arcs(w, h, &skel, (24.0, 80.5));
        assert_eq!(found.len(), 1, "r {r}");
        assert!((found[0].r - r).abs() < 1.5, "{:?}", found[0]);
    }
    // a straight line at a slant is still one run
    let mut skel = vec![false; w * h];
    for x in 5..115 {
        let y = 20.0 + 0.3 * x as f64;
        skel[y as usize * w + x] = true;
    }
    assert_eq!(lines::segments(w, h, &mut skel, 22.5, true).len(), 1);
}

/// A jamb whose line runs on into the last 24 degrees of a swing is no straight run before the
/// swings are read; a straight line with a single step (which the bow test refuses) is read
/// whole by the later pass.
#[test]
fn a_jamb_running_into_a_swing_is_not_a_straight_run() {
    let (w, h) = (80, 80);
    let mut skel = vec![false; w * h];
    // jamb x = 60, rows 70..61 up the image, then the swing (centre 20 px left) bends away
    for y in 61..71 {
        skel[y * w + 60] = true;
    }
    let r = 40.0f64;
    for d in 0..16 {
        let dy = d as f64 + 0.5;
        let x = 60.0 - (r - (r * r - dy * dy).sqrt());
        skel[(60 - d) * w + x.floor() as usize] = true;
    }
    let segs = lines::segments(w, h, &mut skel.clone(), 22.5, true);
    assert!(segs.is_empty(), "{segs:?}");

    let mut skel = vec![false; w * h];
    for y in 10..36 {
        skel[y * w + if y < 27 { 30 } else { 31 }] = true;
    }
    let segs = lines::segments(w, h, &mut skel, 15.0, false);
    assert_eq!(segs.len(), 1, "{segs:?}");
    assert!((segs[0].len() - 26.0).abs() < 1.0, "{segs:?}");
}

/// A pier is read down to 0.4 m less two pixels (its face ends land on pixels), never below
/// half of the drawing setting.
#[test]
fn raster_piers_allow_two_pixels() {
    let v = vector_options().walls.min_length;
    assert!((raster_options(50.0).walls.min_length - (v - 0.04)).abs() < 1e-12);
    assert!((raster_options(200.0).walls.min_length - (v - 0.01)).abs() < 1e-12);
    assert!((raster_options(5.0).walls.min_length - 0.5 * v).abs() < 1e-12);
}

/// Where a door leaf runs on along a jamb, the jamb run takes the last pixels of the faces:
/// a face that stops up to 3 px short of a crossing line is closed onto it, one 5 px short is not.
#[test]
fn faces_stopped_short_of_a_jamb_are_closed_onto_it() {
    let jamb = lines::PxSeg { a: (10.0, 50.0), b: (60.0, 50.0) };
    // 20 degrees off the jamb, its end 1 px below it
    let (c, s) = (20f64.to_radians().cos(), 20f64.to_radians().sin());
    let shallow = lines::PxSeg { a: (35.0 - 20.0 * c, 49.0 - 20.0 * s), b: (35.0, 49.0) };
    let mut segs = vec![
        lines::PxSeg { a: (55.0, 10.0), b: (55.0, 47.6) },
        lines::PxSeg { a: (60.5, 10.0), b: (60.5, 47.6) },
        lines::PxSeg { a: (45.0, 10.0), b: (45.0, 45.0) },
        shallow,
        jamb,
    ];
    lines::close_corners(&mut segs, 3.0);
    let near = |p: (f64, f64), q: (f64, f64)| (p.0 - q.0).abs() < 1e-9 && (p.1 - q.1).abs() < 1e-9;
    assert!(near(segs[0].b, (55.0, 50.0)), "{:?}", segs[0]);
    // the crossing point lies within a pixel past the jamb's end
    assert!(near(segs[1].b, (60.5, 50.0)), "{:?}", segs[1]);
    assert_eq!(segs[2].b, (45.0, 45.0));
    // a shallow meeting is no corner
    assert_eq!(segs[3], shallow);
    assert_eq!(segs[4], jamb);
}

#[test]
fn a_filled_plan_reads_back_walls_and_openings() {
    let p = house();
    let (r, shift) = read(&p, 50.0, WallStyle::Filled, None);
    let s = score(&p, shift, &r.proposals.walls, &r.proposals.openings, 0.02, 0.025);
    assert!(s.wall_iou >= 0.95, "{s:?} {:?}", r.stats);
    assert_eq!(s.openings_found, 4, "{s:?} {:?}", r.proposals.openings);
    assert_eq!(r.stats.arcs, 2, "{:?}", r.stats);
    assert_eq!(r.stats.angle_deg, 0.0);
}

#[test]
fn an_outlined_plan_reads_back_walls_and_openings() {
    let p = house();
    let (r, shift) = read(&p, 100.0, WallStyle::Outline, None);
    let s = score(&p, shift, &r.proposals.walls, &r.proposals.openings, 0.02, 0.025);
    assert!(s.wall_iou >= 0.95, "{s:?} {:?}", r.stats);
    assert_eq!(s.openings_found, 4, "{s:?} {:?}", r.proposals.openings);
}

#[test]
fn a_known_width_gives_the_scale() {
    let p = house();
    let (lo, hi) = p.bounds();
    let (r, shift) = read(&p, 50.0, WallStyle::Filled, Some(ImageScale::Width(hi.0 - lo.0)));
    assert!((r.px_per_m - 50.0).abs() < 0.5, "{}", r.px_per_m);
    let s = score(&p, shift, &r.proposals.walls, &r.proposals.openings, 0.03, 0.03);
    assert!(s.wall_iou >= 0.95, "{s:?}");
}

#[test]
fn calibration_and_print_scale_give_pixels_per_metre() {
    let p = house();
    let (img, _) = p.render(50.0, 1.0, WallStyle::Filled);
    let m = raster::masks(&img);
    let cal = ImageScale::Calibrate { a: (10.0, 10.0), b: (10.0, 210.0), length: 4.0 };
    assert!((px_per_m(&cal, &m, img.w).unwrap() - 50.0).abs() < 1e-9);
    // 127 dpi is 5 px per paper mm; at 1:100 a metre is 10 mm
    let dpi = ImageScale::Dpi { dpi: 127.0, ratio: 100.0 };
    assert!((px_per_m(&dpi, &m, img.w).unwrap() - 50.0).abs() < 1e-9);
    assert!(px_per_m(&ImageScale::PxPerM(0.0), &m, img.w).is_err());
    assert!(read_plan(&Gray::new(30, 30), &ImageScale::PxPerM(50.0), &[], None).is_err());
}

#[test]
fn a_turned_plan_is_read_square_to_its_own_walls() {
    let p = rotated(&house(), 15.0);
    let (r, shift) = read(&p, 100.0, WallStyle::Filled, None);
    assert!((r.stats.angle_deg - 15.0).abs() < 0.2, "{:?}", r.stats);
    let s = score(&p, shift, &r.proposals.walls, &r.proposals.openings, 0.03, 0.03);
    assert!(s.wall_iou >= 0.9, "{s:?} {:?}", r.stats);
}

#[test]
fn the_same_picture_reads_the_same() {
    let p = house();
    let (a, _) = read(&p, 50.0, WallStyle::Filled, None);
    let (b, _) = read(&p, 50.0, WallStyle::Filled, None);
    assert_eq!(a.underlay.lines, b.underlay.lines);
    assert_eq!(a.proposals.walls, b.proposals.walls);
    assert_eq!(a.proposals.openings, b.proposals.openings);
}

#[test]
fn a_vector_sheet_of_the_plan_reads_back_walls_and_openings() {
    let p = house();
    let (sheet, shift) = p.sheet(100.0, 20.0);
    let bytes = export_pdf(&[sheet]).unwrap();
    let page = PdfFile::parse(&bytes).unwrap().page(0).unwrap();
    let opt = ImportOptions { scale: Scale::Ratio(100.0), ..Default::default() };
    let u = Underlay::from_pdf(&page, &opt).unwrap();
    let prop = crate::drawing_model::propose(&u, &[], &vector_options(), &Default::default());
    let s = score(&p, shift, &prop.walls, &prop.openings, 0.02, 0.02);
    assert!(s.wall_iou >= 0.95, "{s:?}");
    assert_eq!(s.openings_found, 4, "{s:?} {:?}", prop.openings);
}

#[test]
fn a_thin_wall_renders_its_full_width_wherever_it_sits() {
    let w = |a: (f64, f64), b: (f64, f64), t: f64| SynthWall { a, b, thickness: t };
    for i in 0..200 {
        let c = 6.6 + i as f64 * 0.035;
        let p = SynthPlan {
            walls: vec![
                w((-5.0, -5.0), (-4.0, -5.0), 0.3),
                w((1.0, c), (4.0, c), 0.1),
                w((c - 3.0, 20.0), (c - 3.0, 23.0), 0.1),
            ],
            openings: vec![],
        };
        let (img, shift) = p.render(50.0, 1.0, WallStyle::Filled);
        let x = ((2.5 + shift.0) * 50.0) as usize;
        let col = (0..img.h).filter(|&y| img.px[y * img.w + x] == 0).count();
        let y = img.h - 1 - ((21.5 + shift.1) * 50.0) as usize;
        let row = (0..img.w).filter(|&x| img.px[y * img.w + x] == 0).count();
        assert_eq!((col, row), (5, 5), "wall at {c}");
    }
}
