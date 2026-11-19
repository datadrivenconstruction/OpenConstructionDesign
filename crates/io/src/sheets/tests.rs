// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use crate::ru_fixtures::ru;
use ddc_model::{LayerSpec, Model, OpeningSpec, PlanViewSpec, RoomSpec, Swing, ViewPrim, WallSpec};

/// A 6×4 m room by outer faces (200 wall inward), door 900, window 1500 — as in model::views.
fn plan() -> PlanView {
    plan_model().0
}

fn plan_model() -> (PlanView, Model) {
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
    let c = [(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)];
    let specs: Vec<WallSpec> = (0..4)
        .map(|k| WallSpec {
            a: c[k],
            b: c[(k + 1) % 4],
            level: lv,
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
    m.add_opening_with(ids[0], &op(1.0, 0.9, 0.0, 2.1, "door"), Swing::default())
        .unwrap();
    m.add_opening(ids[2], &op(2.0, 1.5, 0.9, 1.5, "window"))
        .unwrap();
    m.add_room(&RoomSpec {
        level: lv,
        name: ru("office_word").into(),
        number: "101".into(),
        point: (3.0, 2.0),
        height: 3.0,
    })
    .unwrap();
    let v = m
        .plan_view(&PlanViewSpec {
            level: lv,
            cut_height: 1.2,
            grids: vec![],
        })
        .unwrap();
    (v, m)
}

fn gost_fields() -> BTreeMap<String, String> {
    [
        ("designation", ru("designation_code")),
        ("title", ru("floor_plan")),
        ("organization", ru("org_project")),
        ("developer", ru("ivanov")),
        ("checker", ru("petrov")),
        ("stage", ru("cyr_pe")),
        ("sheet", "1"),
        ("sheets", "3"),
        ("date", "11.26"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

fn a3_gost(view: &PlanView) -> SheetDrawing {
    compose(&SheetSpec {
        paper: "A3".into(),
        landscape: true,
        standard: "gost".into(),
        fields: gost_fields(),
        viewports: vec![Viewport {
            view,
            scale: 50.0,
            rect: None,
            title: ru("plan_floor1").into(),
        }],
        units: LengthUnits::Metric,
        area_unit: None,
        logo: None,
    })
    .unwrap()
}

#[test]
fn paper_and_profiles() {
    assert_eq!(paper_size("A3", true).unwrap(), (420.0, 297.0));
    assert_eq!(paper_size("A4", false).unwrap(), (210.0, 297.0));
    assert_eq!(paper_size("ANSI B", true).unwrap(), (431.8, 279.4));
    assert!(paper_size("B7", true).is_err());
    assert_eq!(papers().len(), 10);
    assert!(standards().contains(&"iso".to_string()) && standards().contains(&"gost".to_string()));
    let g50 = weights("gost", 50.0).unwrap();
    let g100 = weights("gost", 100.0).unwrap();
    let i200 = weights("iso", 200.0).unwrap();
    assert_eq!((g50.cut, g100.cut, i200.cut), (0.8, 0.6, 0.35));
    assert_eq!(
        (weights("iso", 50.0).unwrap().cut, i200.dim_text),
        (0.7, 1.8)
    );
    assert!(weights("ansi-x", 50.0).is_err());
}

#[test]
fn sheet_a3_gost_pdf_roundtrip() {
    let v = plan();
    let s = a3_gost(&v);
    let bytes = s.to_pdf().unwrap();
    assert_eq!(bytes, a3_gost(&v).to_pdf().unwrap(), "deterministic PDF");
    let f = crate::pdf::import::PdfFile::parse(&bytes).unwrap();
    assert_eq!(f.page_count(), 1);
    let p = f.page(0).unwrap();
    assert!((p.width_mm - 420.0).abs() < 0.01 && (p.height_mm - 297.0).abs() < 0.01);
    let texts: Vec<&str> = p.texts.iter().map(|t| t.string.as_str()).collect();
    for want in [
        ru("dev_label"),
        ru("check_label"),
        ru("norm_label"),
        ru("approve_label"),
        // GOST R 21.101 form 3: stage and sheet count; the scale sits in the view title
        "\u{0421}\u{0442}\u{0430}\u{0434}\u{0438}\u{044f}",
        "\u{041a}\u{043e}\u{043b}.\u{0443}\u{0447}.",
        ru("sheet_word"),
        ru("sheets_word"),
        ru("designation_code"),
        ru("floor_plan"),
        ru("org_project"),
        ru("ivanov"),
        ru("petrov"),
        "\u{041f}\u{043b}\u{0430}\u{043d} 1 \u{044d}\u{0442}\u{0430}\u{0436}\u{0430}  1:50",
        ru("office_word"),
        "101",
        ru("area_20_16"),
        "6000",
        "4000",
        "1000",
        "900",
        "4100",
        "2500",
        "1500",
        "2000",
    ] {
        assert!(texts.contains(&want), "missing {want:?} in {texts:?}");
    }
    assert_eq!(texts.iter().filter(|t| **t == "6000").count(), 2);
    assert_eq!(texts.iter().filter(|t| **t == "4000").count(), 2);
}

#[test]
fn sheet_scale_and_frame_geometry() {
    let v = plan();
    let s = a3_gost(&v);
    // South wall section 6000 mm → 120 mm on the sheet at 1:50; GOST frame: 20/5/5/5.
    let frame: Vec<&Vec<[f64; 2]>> = s
        .marks
        .iter()
        .filter_map(|(_, m)| match m {
            Mark::Line { pts, layer, .. } if *layer == FRAME => Some(pts),
            _ => None,
        })
        .collect();
    assert_eq!(frame[1][0], [20.0, 5.0]);
    assert_eq!(frame[1][2], [415.0, 292.0]);
    let cuts: Vec<(f64, f64)> = s
        .marks
        .iter()
        .filter_map(|(_, m)| match m {
            Mark::Line {
                pts, layer, weight, ..
            } if *layer == CUT => {
                let xs = pts.iter().map(|p| p[0]);
                let (lo, hi) = xs.fold((f64::MAX, f64::MIN), |a, x| (a.0.min(x), a.1.max(x)));
                Some((r6(hi - lo), *weight))
            }
            _ => None,
        })
        .collect();
    assert_eq!(cuts.len(), v.count(|p| matches!(p, ViewPrim::Cut { .. })));
    assert!(cuts.iter().all(|c| c.1 == 0.8));
    let total: f64 = cuts.iter().map(|c| c.0).sum();
    assert!(total > 120.0, "{total}");
    // ISO 1:100 is thinner and has a different title block.
    let iso = compose(&SheetSpec {
        paper: "A3".into(),
        landscape: true,
        standard: "iso".into(),
        fields: [("title".to_string(), "Ground floor".to_string())].into(),
        viewports: vec![Viewport {
            view: &v,
            scale: 100.0,
            rect: None,
            title: String::new(),
        }],
        units: LengthUnits::Metric,
        area_unit: None,
        logo: None,
    })
    .unwrap();
    let pdf = iso.to_pdf().unwrap();
    let p = crate::pdf::import::PdfFile::parse(&pdf)
        .unwrap()
        .page(0)
        .unwrap();
    let texts: Vec<&str> = p.texts.iter().map(|t| t.string.as_str()).collect();
    for want in [
        "Legal owner",
        "Identification number",
        "Date of issue",
        "Ground floor",
        "1:100",
        "20.16 m²",
    ] {
        assert!(texts.contains(&want), "missing {want:?}");
    }
}

#[test]
fn sheet_dxf_paper_and_model_space() {
    let v = plan();
    let s = a3_gost(&v);
    let doc = s.to_dxf();
    let txt = crate::dxf::write_string(&doc);
    assert_eq!(
        txt,
        crate::dxf::write_string(&a3_gost(&v).to_dxf()),
        "deterministic DXF"
    );
    let back = crate::dxf::read_str(&txt).unwrap();
    let paper = back
        .blocks
        .iter()
        .find(|b| b.name.eq_ignore_ascii_case("*Paper_Space"))
        .unwrap();
    assert_eq!(paper.entities.len(), s.marks.len());
    let in_view = s.marks.iter().filter(|m| m.0.is_some()).count();
    assert_eq!(back.entities.len(), in_view);
    // Model space 1:1: wall sections in original coordinates.
    let has_text =
        |e: &crate::dxf::Entity, s: &str| matches!(e, crate::dxf::Entity::Text(t) if t.text == s);
    assert!(paper.entities.iter().any(|e| has_text(e, ru("dev_label"))));
    assert!(back.entities.iter().any(|e| has_text(e, ru("area_20_16"))));
    let xs: Vec<f64> = back
        .entities
        .iter()
        .filter_map(|e| match e {
            crate::dxf::Entity::LwPolyline(p) if p.common.layer == "Cut" => {
                Some(p.vertices.iter().map(|v| v.x).fold(f64::MIN, f64::max))
            }
            _ => None,
        })
        .collect();
    let max = xs.iter().cloned().fold(f64::MIN, f64::max);
    assert!((max - 6.0).abs() < 1e-9, "{max}");
}

fn sheet(
    view: &PlanView,
    standard: &str,
    scale: f64,
    fields: BTreeMap<String, String>,
    units: LengthUnits,
) -> SheetDrawing {
    compose(&SheetSpec {
        paper: "A3".into(),
        landscape: true,
        standard: standard.into(),
        fields,
        viewports: vec![Viewport {
            view,
            scale,
            rect: None,
            title: String::new(),
        }],
        units,
        area_unit: None,
        logo: None,
    })
    .unwrap()
}

fn texts(s: &SheetDrawing) -> Vec<&str> {
    s.marks
        .iter()
        .filter_map(|(_, m)| match m {
            Mark::Text { s, .. } => Some(s.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn sheet_metre_model_paper_lengths() {
    assert_eq!(paper_mm(6.0, 100.0), 60.0);
    let v = plan();
    assert_eq!(v.bbox, [0.0, 0.0, 6.0, 4.0]);
    let s = sheet(&v, "iso", 100.0, BTreeMap::new(), LengthUnits::Metric);
    // Overall dimension of the south side 6 m at 1:100 — 60 mm between extension lines.
    let dims: Vec<f64> = s
        .marks
        .iter()
        .filter_map(|(_, m)| match m {
            Mark::Line { pts, layer, .. } if *layer == DIMS && pts.len() == 2 => {
                Some(r6((pts[1][0] - pts[0][0]).abs()))
            }
            _ => None,
        })
        .collect();
    assert!(dims.contains(&60.0), "{dims:?}");
    let t = texts(&s);
    for want in ["6000", "4000", "1000", "900", "4100"] {
        assert!(t.contains(&want), "missing {want} in {t:?}");
    }
    let imp = sheet(&v, "iso", 100.0, BTreeMap::new(), LengthUnits::Imperial);
    let t = texts(&imp);
    assert!(t.contains(&"19'-8 1/4\""), "{t:?}");
    assert_eq!(format_length(0.9, LengthUnits::Imperial), "2'-11 3/8\"");
    assert_eq!(format_length(4.1, LengthUnits::Metric), "4100");
}

#[test]
fn stamp_long_value_fits_cell() {
    let v = plan();
    let long = ru("long_title");
    let mut f = gost_fields();
    f.insert("title".into(), long.into());
    let d = data();
    let st = &d["standards"]["gost"];
    let min_h = num(st, "stamp_min_h");
    let cell = st["stamp"]["cells"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c.get("field").and_then(Value::as_str) == Some("title"))
        .unwrap()
        .clone();
    let s = sheet(&v, "gost", 50.0, f, LengthUnits::Metric);
    let (w, _) = paper_size("A3", true).unwrap();
    let sx = w - 5.0 - num(&st["stamp"], "w");
    let (cx, cw) = (sx + num(&cell, "x"), num(&cell, "w"));
    let (pos, h, txt) = s
        .marks
        .iter()
        .find_map(|(_, m)| match m {
            Mark::Text { pos, h, s, .. } if s.ends_with('…') => Some((*pos, *h, s.clone())),
            _ => None,
        })
        .expect("long title truncated");
    assert!(h >= min_h - 1e-12, "{h}");
    assert!(
        pos[0] >= cx && pos[0] + text_w(&txt, h) <= cx + cw + 1e-9,
        "{txt}"
    );
    assert!(long.starts_with(txt.trim_end_matches('…')));
    // A short value is unchanged.
    assert!(texts(&s).contains(&ru("ivanov")));
}

/// ISO A3 sheet: plan + section through the door + south elevation, 1:100 — PDF and DXF are reproducible.
#[test]
fn sheet_plan_section_elevation_pdf_dxf() {
    use ddc_model::{SectionSpec, ViewKind};
    let (plan, mut m) = plan_model();
    m.add_level("L2", 3.0).unwrap();
    let cut = m
        .section_view(
            &SectionSpec {
                kind: ViewKind::Section,
                name: "1-1".into(),
                a: (1.45, -1.0),
                b: (1.45, 5.0),
                depth: 10.0,
                far_clip: false,
            },
            &[],
        )
        .unwrap();
    assert!(cut.count(|p| matches!(p, ViewPrim::SectionCut { .. })) >= 2);
    let ids = m.add_compass_elevations(3.0).unwrap();
    let south = m.draw_view_def(ids[1], &[]).unwrap();
    assert!(south.count(|p| matches!(p, ViewPrim::Edge { .. })) > 0);
    let compose_it = || {
        let vp = |view, x: f64, title: &str| Viewport {
            view,
            scale: 100.0,
            rect: Some([x, 80.0, 125.0, 150.0]),
            title: title.into(),
        };
        compose(&SheetSpec {
            paper: "A3".into(),
            landscape: true,
            standard: "iso".into(),
            fields: BTreeMap::new(),
            viewports: vec![
                vp(&plan, 20.0, "Plan"),
                vp(&cut, 150.0, "Section 1-1"),
                vp(&south, 280.0, "South"),
            ],
            units: LengthUnits::Metric,
            area_unit: None,
            logo: None,
        })
        .unwrap()
    };
    let s = compose_it();
    let pdf = s.to_pdf().unwrap();
    assert_eq!(pdf, compose_it().to_pdf().unwrap(), "deterministic PDF");
    let f = crate::pdf::import::PdfFile::parse(&pdf).unwrap();
    assert_eq!(f.page_count(), 1);
    let p = f.page(0).unwrap();
    let texts: Vec<&str> = p.texts.iter().map(|t| t.string.as_str()).collect();
    for want in ["L1", "L2", "±0.000", "+3.000", "3000"] {
        assert!(texts.contains(&want), "missing {want:?} in {texts:?}");
    }
    let txt = crate::dxf::write_string(&s.to_dxf());
    assert_eq!(txt, crate::dxf::write_string(&compose_it().to_dxf()));
    let back = crate::dxf::read_str(&txt).unwrap();
    let on = |l: &str| {
        back.entities
            .iter()
            .filter(|e| match e {
                crate::dxf::Entity::Line(x) => x.common.layer == l,
                crate::dxf::Entity::LwPolyline(x) => x.common.layer == l,
                _ => false,
            })
            .count()
    };
    for l in ["Cut", "Hatch", "Projection", "Levels"] {
        assert!(on(l) > 0, "empty layer {l}");
    }
}

/// Dimension objects on the sheet: auto dimensions 6×4, arc, angle, level mark; two tags without overlap.
#[test]
fn sheet_model_dims_exact_and_no_label_overlap() {
    use ddc_model::{DimKind, DimRef, DimSpec};
    let (_, mut m) = plan_model();
    let lv = 1;
    m.add_room(&RoomSpec {
        level: lv,
        name: "B".into(),
        number: "102".into(),
        point: (3.1, 2.0),
        height: 3.0,
    })
    .unwrap();
    assert_eq!(m.auto_dimension(lv).unwrap().len(), 10);
    let mut sp = DimSpec {
        level: lv,
        kind: DimKind::Spot,
        refs: vec![
            DimRef::WallLevel { wall: 1, top: true },
            DimRef::Point((1.0, 1.0)),
        ],
        dir: (0.0, 0.0),
        side: 1.0,
        offset: 0.0,
        tier: -1,
        auto: false,
    };
    m.add_dim(&sp).unwrap();
    sp.kind = DimKind::Angular;
    sp.refs = vec![DimRef::AxisLine { wall: 1 }, DimRef::AxisLine { wall: 2 }];
    sp.offset = 0.5;
    m.add_dim(&sp).unwrap();
    let v = m
        .plan_view(&PlanViewSpec {
            level: lv,
            cut_height: 1.2,
            grids: vec![],
        })
        .unwrap();
    let make = || {
        compose(&SheetSpec {
            paper: "A3".into(),
            landscape: true,
            standard: "iso".into(),
            fields: BTreeMap::new(),
            viewports: vec![Viewport {
                view: &v,
                scale: 50.0,
                rect: Some([20.0, 80.0, 300.0, 200.0]),
                title: String::new(),
            }],
            units: LengthUnits::Metric,
            area_unit: None,
            logo: None,
        })
        .unwrap()
    };
    let s = make();
    let dims: Vec<([f64; 2], f64, String)> = s
        .marks
        .iter()
        .filter_map(|(vp, mk)| match mk {
            Mark::Text {
                pos, rot, s, layer, ..
            } if vp.is_some() && *layer == DIMS => Some((*pos, *rot, s.clone())),
            _ => None,
        })
        .collect();
    let mut strs: Vec<&str> = dims.iter().map(|d| d.2.as_str()).collect();
    strs.sort();
    assert_eq!(
        strs,
        [
            "+3.000", "1000", "1500", "2000", "2500", "4000", "4000", "4100", "6000", "6000",
            "90.00°", "900"
        ]
    );
    // south overall: line 18 mm (first 10 + step 8) below the face y = 140 mm, text below it, centred
    let six: Vec<&([f64; 2], f64, String)> = dims.iter().filter(|d| d.2 == "6000").collect();
    let south = six.iter().find(|d| d.0[1] < 140.0).unwrap();
    assert_eq!(south.0, [r6(170.0 - 5.72 / 2.0), 119.0]);
    assert_eq!(south.1, 0.0);
    let line = s.marks.iter().any(|(_, mk)| {
        matches!(mk, Mark::Line { pts, layer, .. } if *layer == DIMS && pts.as_slice() == [[230.0, 122.0], [110.0, 122.0]])
    });
    assert!(line, "south overall dimension line");
    // all texts of the viewport are pairwise free of overlaps
    let rects: Vec<_> = s
        .marks
        .iter()
        .filter_map(|(vp, mk)| match mk {
            Mark::Text { pos, h, rot, s, .. } if vp.is_some() => {
                Some(annot::text_rect(*pos, *h, *rot, s))
            }
            _ => None,
        })
        .collect();
    assert_eq!(ddc_geom::label::total_overlap(&rects), 0.0);
    // the first tag keeps its place (centre 170, 180 mm), the second one leaves its natural spot
    let tags: Vec<[f64; 2]> = s
        .marks
        .iter()
        .filter_map(|(_, mk)| match mk {
            Mark::Text { pos, s, layer, .. } if *layer == TAGS && s == "102" => Some(*pos),
            _ => None,
        })
        .collect();
    let first: Vec<[f64; 2]> = s
        .marks
        .iter()
        .filter_map(|(_, mk)| match mk {
            Mark::Text { pos, s, layer, .. } if *layer == TAGS && s == "101" => Some(*pos),
            _ => None,
        })
        .collect();
    // four lines at 1:50 (name, number, area, floor finish: within tag_finish_max_scale)
    assert_eq!(first[0], [r6(170.0 - text_width("101", 3.5) / 2.0), 180.0]);
    assert_ne!(tags[0], [r6(172.0 - text_width("102", 3.5) / 2.0), 180.0]);
    assert_eq!(make().to_pdf().unwrap(), s.to_pdf().unwrap());
    let a = crate::dxf::write_string(&s.to_dxf());
    assert_eq!(a, crate::dxf::write_string(&make().to_dxf()));
}

fn logo_px(w: u32, h: u32) -> crate::logo::Logo {
    let px = (0..w * h).flat_map(|_| [20u8, 90, 160, 200]).collect();
    crate::logo::Logo::from_rgba(w, h, px).unwrap()
}

fn with_logo(standard: &str, logo: Option<crate::logo::Logo>) -> SheetDrawing {
    let v = plan();
    compose(&SheetSpec {
        paper: "A3".into(),
        landscape: true,
        standard: standard.into(),
        fields: [("organization".to_string(), "ACME".to_string())].into(),
        viewports: vec![Viewport {
            view: &v,
            scale: 100.0,
            rect: None,
            title: String::new(),
        }],
        units: LengthUnits::Metric,
        area_unit: None,
        logo,
    })
    .unwrap()
}

#[test]
fn logo_sits_inside_the_owner_cell_of_both_standards() {
    for (standard, cell) in [("gost", "organization"), ("iso", "legal_owner")] {
        let d = with_logo(standard, Some(logo_px(300, 100)));
        let lg = d.logo.as_ref().unwrap_or_else(|| panic!("{standard}: no logo"));
        let c = stamp_cells(standard, "A3", true)
            .unwrap()
            .into_iter()
            .find(|c| c.key == cell)
            .unwrap();
        let [x, y, w, h] = lg.rect;
        let r = c.rect;
        assert!(x >= r[0] && y >= r[1], "{standard}: starts outside the cell");
        assert!(x + w <= r[0] + r[2] && y + h <= r[1] + r[3], "{standard}: leaves the cell");
        assert!((w / h - 3.0).abs() < 1e-6, "{standard}: proportions kept");
        // the cell's text starts right of the picture
        let text_x = d
            .marks
            .iter()
            .filter_map(|(_, m)| match m {
                Mark::Text { pos, s, .. } if s == "ACME" => Some(pos[0]),
                _ => None,
            })
            .next();
        if standard == "gost" {
            assert!(text_x.unwrap() >= x + w, "gost: text under the picture");
        }
    }
}

#[test]
fn no_logo_means_no_image_and_the_text_stays_put() {
    let d = with_logo("gost", None);
    assert!(d.logo.is_none());
    assert!(!d.to_pdf_sheet().items.iter().any(|i| matches!(i, crate::pdf::Item::Image { .. })));
    let pdf = d.to_pdf().unwrap();
    assert!(!pdf.windows(15).any(|w| w == b"/Subtype /Image"));
    let c = stamp_cells("gost", "A3", true).unwrap();
    let org = c.iter().find(|c| c.key == "organization").unwrap();
    let x = d
        .marks
        .iter()
        .find_map(|(_, m)| match m {
            Mark::Text { pos, s, .. } if s == "ACME" => Some(pos[0]),
            _ => None,
        })
        .unwrap();
    assert!((x - (org.rect[0] + 1.0)).abs() < 1e-9);
}

#[test]
fn pdf_of_a_sheet_carries_the_logo_image_with_its_transparency() {
    let d = with_logo("gost", Some(logo_px(120, 60)));
    let pdf = d.to_pdf().unwrap();
    assert!(pdf.windows(15).any(|w| w == b"/Subtype /Image"));
    assert!(pdf.windows(6).any(|w| w == b"/SMask"));
    let item = d
        .to_pdf_sheet()
        .items
        .into_iter()
        .find(|i| matches!(i, crate::pdf::Item::Image { .. }));
    assert!(item.is_some());
}

#[test]
fn fitting_the_sheet_to_a_smaller_paper_scales_the_logo_with_it() {
    let mut d = with_logo("gost", Some(logo_px(120, 60)));
    let before = d.logo.as_ref().unwrap().rect;
    let (w, h) = (d.width / 2.0, d.height / 2.0);
    d.fit_to(w, h);
    let after = d.logo.as_ref().unwrap().rect;
    assert!((after[2] - before[2] / 2.0).abs() < 1e-9);
    assert!((after[0] - before[0] / 2.0).abs() < 1e-9);
}
