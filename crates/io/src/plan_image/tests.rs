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
    let segs = lines::segments(w, h, &mut skel, 30.0);
    assert_eq!(segs.len(), 1, "{segs:?}");
    let s = segs[0];
    let (x0, x1) = (s.a.0.min(s.b.0), s.a.0.max(s.b.0));
    assert!((x0 - 20.0).abs() < 1.0 && (x1 - 180.0).abs() < 1.0, "{s:?}");
    assert!((s.a.1 - 50.5).abs() < 0.2 && (s.b.1 - 50.5).abs() < 0.2, "{s:?}");
    let arcs = arcs::arcs(w, h, &skel, (30.0, 60.0));
    assert_eq!(arcs.len(), 1);
    assert!((arcs[0].r - 45.0).abs() < 1.5, "{:?}", arcs[0]);
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
