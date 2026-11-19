//! Sheet issue: schedule in columns within sheet assembly, link to the model, title block from project fields,
//! cross-references after renumbering, multi-page PDF, line weights and hatching.

use super::*;
use crate::pdf::import::PdfFile;
use crate::ru_fixtures::ru;
use ddc_model::{
    LayerSpec, Model, Revision, SchedulePlace, ScheduleSplit, SectionSpec, SheetDef, ViewKind,
    ViewRef, ViewportDef, WallSpec, DEFAULT_CUT,
};

fn house() -> (Model, i64, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let wt = m
        .add_wall_type(
            "W",
            &[LayerSpec {
                material: "Concrete".into(),
                thickness: 0.25,
            }],
        )
        .unwrap();
    let c = [(0.0, 0.0), (8.0, 0.0), (8.0, 5.0), (0.0, 5.0)];
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
    m.add_walls(&specs).unwrap();
    let sec = m
        .add_view_def(&SectionSpec {
            kind: ViewKind::Section,
            name: "1-1".into(),
            a: (4.0, -1.0),
            b: (4.0, 6.0),
            depth: 10.0,
            far_clip: false,
        })
        .unwrap();
    (m, lv, sec)
}

fn long(rows: usize) -> impl Fn(&str) -> Option<Table> {
    move |key: &str| {
        (key == "doors").then(|| Table {
            title: ru("doors_word").into(),
            header: vec!["№".into(), ru("mark_word").into()],
            rows: (0..rows)
                .map(|i| vec![format!("{}", i + 1), format!("{}-{i}", ru("cyr_de"))])
                .collect(),
            swatches: vec![],
        })
    }
}

/// Catalog strings seen by the assembly tests (file `i18n/ru/sheets_issue.json` lives in the UI crate).
pub(super) fn ru_text(k: &str) -> String {
    // the strings live in tests/data/ru_fixtures.tsv under `cat.<key>`; a missing key panics
    let key = k.strip_prefix("sheets.paper.").unwrap_or(k);
    ru(&format!("cat.{key}")).to_string()
}

fn ctx(f: &dyn Fn(&str) -> Option<Table>) -> Ctx<'_> {
    Ctx {
        units: LengthUnits::Metric,
        schedules: f,
        index: None,
        paper: None,
        text: &ru_text,
        refs: None,
    }
}

/// Project fields in the store written by the project information dialog.
fn set_project(m: &Model, pairs: &[(&str, &str)]) {
    for (k, v) in pairs {
        m.sql()
            .execute(
                "INSERT INTO prop(kind, ref, key, value, unit) VALUES ('project_info', 0, ?1, ?2, '')",
                [k, v],
            )
            .unwrap();
    }
}

fn plan(sheet: i64, level: i64, scale: f64) -> ViewportDef {
    ViewportDef {
        sheet,
        view: ViewRef::Plan {
            level,
            cut: DEFAULT_CUT,
        },
        scale,
        center: (170.0, 170.0),
        crop: None,
        title: ru("plan_cap").into(),
    }
}

fn section_on(sheet: i64, sec: i64) -> ViewportDef {
    ViewportDef {
        sheet,
        view: ViewRef::View(sec),
        scale: 50.0,
        center: (150.0, 170.0),
        crop: None,
        title: "1-1".into(),
    }
}

fn texts(d: &SheetDrawing, vp: Option<usize>) -> Vec<String> {
    d.marks
        .iter()
        .filter(|(v, _)| vp.is_none() || *v == vp)
        .filter_map(|(_, m)| match m {
            Mark::Text { s, .. } => Some(s.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn schedule_on_sheet_paginates_and_stays_linked() {
    let (mut m, _, _) = house();
    let s = m.add_sheet(&SheetDef::new("A-101", ru("schedules_word"))).unwrap();
    let p = m
        .add_schedule_place(&SchedulePlace {
            sheet: s,
            schedule: "doors".into(),
            pos: (30.0, 280.0),
        })
        .unwrap();
    let f = long(120);
    let c = compose_sheet(&m, s, &ctx(&f)).unwrap();
    let got = texts(&c.drawing, None);
    for i in 0..120 {
        let want = format!("{}-{i}", ru("cyr_de"));
        assert_eq!(got.iter().filter(|t| **t == want).count(), 1, "{want}");
    }
    // A3: 120 rows do not fit one frame column, the header repeats
    let auto = got.iter().filter(|t| *t == ru("mark_word")).count();
    assert!(auto >= 2, "columns: {auto}");
    let r_auto = c.placed[0].rect;
    assert!(r_auto[1] >= 10.0 - 1e-6, "above the frame: {r_auto:?}");

    // a set column height yields more columns; undo restores the automatic one
    m.set_schedule_split(
        p,
        ScheduleSplit {
            max_h: 60.0,
            gap: 5.0,
        },
    )
    .unwrap();
    let c2 = compose_sheet(&m, s, &ctx(&f)).unwrap();
    let narrow = texts(&c2.drawing, None)
        .iter()
        .filter(|t| *t == ru("mark_word"))
        .count();
    assert!(narrow > auto, "{narrow} > {auto}");
    assert!(c2.placed[0].rect[3] <= 60.0 + 1e-6);
    m.undo().unwrap();

    // the table is computed at assembly: different model data — a different sheet
    let f2 = long(3);
    let c3 = compose_sheet(&m, s, &ctx(&f2)).unwrap();
    let t3 = texts(&c3.drawing, None);
    assert_eq!(t3.iter().filter(|t| *t == ru("mark_word")).count(), 1);
    assert!(!t3.iter().any(|t| *t == format!("{}-3", ru("cyr_de"))));
}

#[test]
fn schedule_column_over_the_stamp_stops_at_its_top() {
    let (mut m, _, _) = house();
    let s = m.add_sheet(&SheetDef::new("A-101", ru("schedules_word"))).unwrap();
    // A3 landscape, ISO title block 180×36 in the frame's bottom-right corner: left edge x=230, top y=46
    m.add_schedule_place(&SchedulePlace {
        sheet: s,
        schedule: "doors".into(),
        pos: (300.0, 280.0),
    })
    .unwrap();
    let f = long(2000);
    let c = compose_sheet(&m, s, &ctx(&f)).unwrap();
    let r = c.placed[0].rect;
    assert!(r[1] >= 46.0 - 1e-6, "{r:?}");
    assert!(r[0] + r[2] <= 410.0 + 1e-6, "right frame: {r:?}");
    // what did not fit is counted on the sheet and in build messages: on sheet + shown = all rows
    let got = texts(&c.drawing, None);
    let shown = (0..2000)
        .filter(|i| got.iter().any(|t| *t == format!("{}-{i}", ru("cyr_de"))))
        .count();
    assert!(shown > 0 && shown < 2000);
    let n = 2000 - shown;
    assert!(
        got.iter().any(|t| *t == format!("{}: {n}", ru("rows_not_shown"))),
        "{n}"
    );
    assert!(
        c.warnings.iter().any(|w| w.contains(&n.to_string())),
        "{:?}",
        c.warnings
    );
    // what fit entirely reports nothing
    let few = long(3);
    assert!(compose_sheet(&m, s, &ctx(&few))
        .unwrap()
        .warnings
        .is_empty());
}

#[test]
fn sheet_list_is_linked_to_sheets_and_revisions() {
    let (mut m, _, _) = house();
    let a = m.add_sheet(&SheetDef::new("A-101", ru("title_sheet"))).unwrap();
    let b = m.add_sheet(&SheetDef::new("A-102", ru("plans_word"))).unwrap();
    m.add_schedule_place(&SchedulePlace {
        sheet: a,
        schedule: SHEET_LIST.into(),
        pos: (30.0, 280.0),
    })
    .unwrap();
    m.set_revisions(
        b,
        &[Revision {
            mark: "7".into(),
            date: "05.11.26".into(),
            ..Default::default()
        }],
    )
    .unwrap();
    let f = long(0);
    let got = texts(&compose_sheet(&m, a, &ctx(&f)).unwrap().drawing, None);
    for want in [
        ru("cat.sheet_list"),
        "A-101",
        "A-102",
        ru("plans_word"),
        "7",
        "05.11.26",
    ] {
        assert!(got.iter().any(|t| t == want), "{want} in {got:?}");
    }
    m.renumber_sheets(&[b], "B-5").unwrap();
    let got = texts(&compose_sheet(&m, a, &ctx(&f)).unwrap().drawing, None);
    assert!(got.iter().any(|t| t == "B-5"));
    assert!(!got.iter().any(|t| t == "A-102"));

    // set list: only its sheets; a deleted set — "no data"
    let set = m.add_sheet_set("S", &[b]).unwrap();
    let key = format!("sheet_list:{set}");
    m.add_schedule_place(&SchedulePlace {
        sheet: b,
        schedule: key,
        pos: (30.0, 280.0),
    })
    .unwrap();
    let got = texts(&compose_sheet(&m, b, &ctx(&f)).unwrap().drawing, None);
    assert!(!got.iter().any(|t| t == ru("title_sheet")), "{got:?}");
    m.delete_sheet_set(set).unwrap();
    let got = texts(&compose_sheet(&m, b, &ctx(&f)).unwrap().drawing, None);
    assert!(got.iter().any(|t| t == ru("cat.no_data")), "{got:?}");
    assert!(!got.iter().any(|t| t.starts_with("sheet_list")), "{got:?}");
}

#[test]
fn legends_draw_samples_from_model_and_standard() {
    let (mut m, lv, _) = house();
    let s = m.add_sheet(&SheetDef::new("A-101", ru("legends_word"))).unwrap();
    for (k, key) in LEGENDS.iter().enumerate() {
        m.add_schedule_place(&SchedulePlace {
            sheet: s,
            schedule: key.to_string(),
            pos: (30.0 + 110.0 * k as f64, 280.0),
        })
        .unwrap();
    }
    let f = long(0);
    let c = compose_sheet(&m, s, &ctx(&f)).unwrap();
    let got = texts(&c.drawing, None);
    for want in [
        ru("cat.materials"),
        "Concrete",
        ru("cat.wall_types"),
        "W",
        "250",
        ru("cat.lines"),
        "0.13",
    ] {
        assert!(got.iter().any(|t| t == want), "{want} in {got:?}");
    }
    // concrete sample — hatching, no fills
    assert!(!c
        .drawing
        .marks
        .iter()
        .any(|(_, x)| matches!(x, Mark::Fill { .. })));
    // line sample — profile weight for the sheet scale (0.7 appears nowhere else in the tables)
    let cut = weights("iso", 50.0).unwrap().cut;
    assert_eq!(cut, 0.7);
    m.add_viewport(&plan(s, lv, 50.0)).unwrap();
    let c1 = compose_sheet(&m, s, &ctx(&f)).unwrap();
    assert!(c1.drawing.marks.iter().any(|(v, x)| {
        v.is_none()
            && matches!(x, Mark::Line { weight, layer, .. }
                if *layer == SCHED && (*weight - cut).abs() < 1e-9)
    }));
    // 1:50 next to 1:200: the legend shows hatching, it is on the sheet and identifies the 1:50 view
    let legend_fill = |c: &crate::sheets::Composed| {
        c.drawing
            .marks
            .iter()
            .any(|(v, x)| v.is_none() && matches!(x, Mark::Fill { .. }))
    };
    m.add_viewport(&plan(s, lv, 200.0)).unwrap();
    assert!(!legend_fill(&compose_sheet(&m, s, &ctx(&f)).unwrap()));
    // on a sheet with only the small scale — a fill, as on the plan itself
    let small = m.add_sheet(&SheetDef::new("A-102", ru("small_word"))).unwrap();
    m.add_viewport(&plan(small, lv, 200.0)).unwrap();
    m.add_schedule_place(&SchedulePlace {
        sheet: small,
        schedule: LEGEND_MATERIALS.to_string(),
        pos: (30.0, 280.0),
    })
    .unwrap();
    assert!(legend_fill(&compose_sheet(&m, small, &ctx(&f)).unwrap()));
}

#[test]
fn project_fields_fill_the_stamp_and_sheet_value_wins() {
    let (mut m, _, _) = house();
    let mut d = SheetDef::new("A-101", ru("plan_cap"));
    d.standard = "gost".into();
    let a = m.add_sheet(&d).unwrap();
    let mut o = SheetDef::new("A-102", ru("plan_2"));
    o.standard = "gost".into();
    o.fields.insert("organization".into(), ru("own_word").into());
    let b = m.add_sheet(&o).unwrap();
    set_project(
        &m,
        &[
            ("organization", ru("org_project")),
            ("number", ru("ddc_01")),
            ("author", ru("ivanov")),
        ],
    );
    let f = long(0);
    let t = texts(&compose_sheet(&m, a, &ctx(&f)).unwrap().drawing, None);
    for want in [ru("org_project"), ru("ddc_01"), ru("ivanov")] {
        assert!(t.iter().any(|x| x == want), "{want} in {t:?}");
    }
    let t = texts(&compose_sheet(&m, b, &ctx(&f)).unwrap().drawing, None);
    assert!(t.iter().any(|x| x == ru("own_word")));
    assert!(!t.iter().any(|x| x == ru("org_project")));
}

#[test]
fn project_keys_map_to_iso_cells_and_never_replace_automatic_ones() {
    let (mut m, _, _) = house();
    let s = m.add_sheet(&SheetDef::new("A-101", ru("plan_cap"))).unwrap();
    m.set_revisions(
        s,
        &[ddc_model::Revision {
            mark: "1".into(),
            date: "04.11.26".into(),
            description: ru("issue_word").into(),
            author: ru("initials_ai").into(),
        }],
    )
    .unwrap();
    set_project(
        &m,
        &[
            ("name", ru("school_word")),
            ("client", ru("client_word")),
            ("number", ru("p7")),
            ("issue_date", "01.01.20"),
            ("title", ru("foreign_title")),
        ],
    );
    let f = long(0);
    let t = texts(&compose_sheet(&m, s, &ctx(&f)).unwrap().drawing, None);
    for want in [ru("school_word"), ru("client_word"), ru("p7"), ru("plan_cap"), "04.11.26"] {
        assert!(t.iter().any(|x| x == want), "{want} in {t:?}");
    }
    for no in ["01.01.20", ru("foreign_title")] {
        assert!(!t.iter().any(|x| x == no), "{no} in {t:?}");
    }
}

#[test]
fn stamp_cells_match_the_composed_title_block() {
    let cells = stamp_cells("iso", "A3", true).unwrap();
    let c = cells.iter().find(|c| c.key == "title").unwrap();
    assert_eq!(c.label, "Title");
    // frame: right edge 410, bottom 10; title block 180×36; Title cell x=45, y=0, 90×18
    assert_eq!(c.rect, [275.0, 10.0, 90.0, 18.0]);
    assert!(cells.iter().all(|c| !c.key.is_empty()));
    let gost = stamp_cells("gost", "A3", true).unwrap();
    assert!(gost.iter().any(|c| c.key == "developer"));
    assert!(stamp_cells("nope", "A3", true).is_err());
}

#[test]
fn cross_references_update_on_renumber_and_move() {
    let (mut m, lv, sec) = house();
    let a = m.add_sheet(&SheetDef::new("A-101", ru("plan_cap"))).unwrap();
    let b = m.add_sheet(&SheetDef::new("A-201", ru("sections_word"))).unwrap();
    let c_sheet = m.add_sheet(&SheetDef::new("A-301", ru("other_word"))).unwrap();
    m.add_viewport(&plan(a, lv, 100.0)).unwrap();
    let f = long(0);
    // marks only: the plan's own number in its caption (Titles layer) is not counted
    let marks = |m: &Model| -> Vec<String> {
        compose_sheet(m, a, &ctx(&f))
            .unwrap()
            .drawing
            .marks
            .iter()
            .filter_map(|(v, x)| match x {
                Mark::Text { s, layer, .. } if *v == Some(0) && *layer == TAGS => Some(s.clone()),
                _ => None,
            })
            .collect()
    };

    // the view is not on a sheet: the mark shows only its name
    let t = marks(&m);
    assert_eq!(t.iter().filter(|x| *x == "1-1").count(), 2, "{t:?}");
    assert!(!t.iter().any(|x| x == "A-201"));

    // placed: view number and sheet number at each end of the section line
    let vp = m.add_viewport(&section_on(b, sec)).unwrap();
    let t = marks(&m);
    assert_eq!(t.iter().filter(|x| *x == "A-201").count(), 2, "{t:?}");
    assert_eq!(t.iter().filter(|x| *x == "1").count(), 2, "{t:?}");

    // sheet renumbering
    m.renumber_sheets(&[b], "C-7").unwrap();
    let t = marks(&m);
    assert_eq!(t.iter().filter(|x| *x == "C-7").count(), 2, "{t:?}");
    assert!(!t.iter().any(|x| x == "A-201"));
    m.undo().unwrap();
    assert_eq!(marks(&m).iter().filter(|x| *x == "A-201").count(), 2);

    // manual view number and moving the view to another sheet
    m.set_viewport_detail(vp, "12").unwrap();
    assert_eq!(marks(&m).iter().filter(|x| *x == "12").count(), 2);
    let mut d = m.viewport(vp).unwrap();
    d.sheet = c_sheet;
    m.update_viewport(vp, &d).unwrap();
    let t = marks(&m);
    assert_eq!(t.iter().filter(|x| *x == "A-301").count(), 2, "{t:?}");
    assert_eq!(t.iter().filter(|x| *x == "12").count(), 2, "{t:?}");

    // removed from the sheet: only the name again
    m.delete_viewport(vp).unwrap();
    assert_eq!(marks(&m).iter().filter(|x| *x == "1-1").count(), 2);
}

#[test]
fn view_title_shows_number_name_and_scale() {
    let (mut m, lv, sec) = house();
    let a = m.add_sheet(&SheetDef::new("A-101", ru("plan_cap"))).unwrap();
    m.add_viewport(&plan(a, lv, 100.0)).unwrap();
    let mut second = section_on(a, sec);
    second.center = (330.0, 170.0);
    m.add_viewport(&second).unwrap();
    let f = long(0);
    let d = compose_sheet(&m, a, &ctx(&f)).unwrap().drawing;
    let titles = |k: usize| -> Vec<String> {
        d.marks
            .iter()
            .filter_map(|(v, x)| match x {
                Mark::Text { s, layer, .. } if *v == Some(k) && *layer == TITLES => Some(s.clone()),
                _ => None,
            })
            .collect()
    };
    assert_eq!(titles(0), ["1", ru("plan_cap"), "1:100"]);
    assert_eq!(titles(1), ["2", "1-1", "1:50"]);
}

#[test]
fn section_mark_stays_inside_crop_or_disappears() {
    let (mut m, lv, sec) = house();
    let a = m.add_sheet(&SheetDef::new("A-101", ru("plan_cap"))).unwrap();
    let b = m.add_sheet(&SheetDef::new("A-201", ru("sections_word"))).unwrap();
    m.add_viewport(&section_on(b, sec)).unwrap();
    let mut v = plan(a, lv, 50.0);
    v.crop = Some([-1.0, -2.0, 9.0, 7.0]);
    let vid = m.add_viewport(&v).unwrap();
    let f = long(0);
    let t = texts(&compose_sheet(&m, a, &ctx(&f)).unwrap().drawing, Some(0));
    assert_eq!(t.iter().filter(|x| *x == "A-201").count(), 2, "{t:?}");
    // crop misses the section line: no mark
    v.crop = Some([5.0, 0.0, 8.0, 5.0]);
    m.update_viewport(vid, &v).unwrap();
    let t = texts(&compose_sheet(&m, a, &ctx(&f)).unwrap().drawing, Some(0));
    assert!(!t.iter().any(|x| x == "A-201"), "{t:?}");
}

#[test]
fn set_pdf_has_every_sheet_with_its_number() {
    let (mut m, lv, sec) = house();
    let mut ids = vec![];
    for k in 0..5 {
        let number = format!("A-{}", 101 + k);
        let id = m.add_sheet(&SheetDef::new(&number, ru("sheet_word"))).unwrap();
        m.add_viewport(&plan(id, lv, 100.0)).unwrap();
        ids.push(id);
    }
    m.add_viewport(&section_on(ids[4], sec)).unwrap();
    let set = m.add_sheet_set(ru("issue_word"), &ids[1..4]).unwrap();
    let f = long(0);
    let c = ctx(&f);
    let page_texts = |pdf: &PdfFile, k: usize| -> Vec<String> {
        pdf.page(k)
            .unwrap()
            .texts
            .iter()
            .map(|t| t.string.clone())
            .collect()
    };
    let pdf = PdfFile::parse(&export_sheets_pdf(&m, &ids, &c).unwrap()).unwrap();
    assert_eq!(pdf.page_count(), 5);
    for k in 0..5 {
        let want = format!("A-{}", 101 + k);
        let got = page_texts(&pdf, k);
        assert!(got.iter().any(|t| *t == want), "page {k}: {got:?}");
        assert!(got.iter().any(|t| *t == format!("{}/5", k + 1)), "{got:?}");
    }
    // set: only its sheets, "Sheet N of M" counted within the set
    let members = m
        .sheet_sets()
        .unwrap()
        .into_iter()
        .find(|s| s.0 == set)
        .unwrap()
        .2;
    let pdf = PdfFile::parse(&export_sheets_pdf(&m, &members, &c).unwrap()).unwrap();
    assert_eq!(pdf.page_count(), 3);
    assert!(page_texts(&pdf, 0).iter().any(|t| t == "A-102"));
    assert!(page_texts(&pdf, 2).iter().any(|t| t == "3/3"));
}

#[test]
fn vector_output_keeps_weights_and_hatch_in_pdf_and_dxf() {
    let (mut m, lv, _) = house();
    let a = m.add_sheet(&SheetDef::new("A-101", ru("plan_cap"))).unwrap();
    m.add_viewport(&plan(a, lv, 50.0)).unwrap();
    let f = long(0);
    let d = print_sheet(&m, a, &ctx(&f)).unwrap();
    let w = weights("iso", 50.0).unwrap();
    let hatch = d
        .marks
        .iter()
        .filter(|(_, x)| {
            matches!(x, Mark::Line { layer, weight, .. }
                if *layer == HATCH && (*weight - w.hatch).abs() < 1e-9)
        })
        .count();
    assert!(hatch > 10, "concrete hatch lines: {hatch}");

    // PDF: the same weights, hatching — lines
    let pdf = PdfFile::parse(&to_pdf(&[d.to_pdf_sheet()]).unwrap()).unwrap();
    let page = pdf.page(0).unwrap();
    let near = |t: f64| {
        page.shapes
            .iter()
            .any(|s| s.stroke && (s.width_mm - t).abs() < 5e-3)
    };
    assert!(near(w.cut) && near(w.hatch), "pdf weights");
    assert!(page.shapes.iter().filter(|s| s.stroke).count() > 10);

    // DXF: weight on every object, weight display on, hatching on its own layer
    let dxf = d.to_dxf();
    let standard = dxf.styles.iter().find(|t| t.name == "Standard").unwrap();
    assert!(standard.font.ends_with(".ttf"), "{}", standard.font);
    assert!(dxf.header.iter().any(|h| h.name == "$LWDISPLAY"));
    let paper = &dxf.blocks[0].entities;
    let weight_of = |e: &crate::dxf::Entity| match e {
        crate::dxf::Entity::Line(l) => l.common.lineweight,
        crate::dxf::Entity::LwPolyline(l) => l.common.lineweight,
        _ => None,
    };
    let has = |lw: i32| paper.iter().any(|e| weight_of(e) == Some(lw));
    assert!(has((w.cut * 100.0).round() as i32), "cut weight");
    assert!(has((w.hatch * 100.0).round() as i32), "hatch weight");
    let on_hatch = paper
        .iter()
        .filter(|e| matches!(e, crate::dxf::Entity::Line(l) if l.common.layer == "Hatch"))
        .count();
    assert!(on_hatch > 10, "{on_hatch}");

    // small scale: sections are filled in both formats
    let b = m.add_sheet(&SheetDef::new("A-102", ru("site_plan"))).unwrap();
    m.add_viewport(&plan(b, lv, 200.0)).unwrap();
    let d2 = print_sheet(&m, b, &ctx(&f)).unwrap();
    let pdf2 = PdfFile::parse(&to_pdf(&[d2.to_pdf_sheet()]).unwrap()).unwrap();
    assert!(pdf2.page(0).unwrap().shapes.iter().any(|s| s.fill));
    let hatches = d2.to_dxf().blocks[0]
        .entities
        .iter()
        .filter(|e| matches!(e, crate::dxf::Entity::Unknown(p) if p[0].value == "HATCH"))
        .count();
    assert!(hatches > 0, "{hatches}");
}

#[test]
fn sheet_has_measures() {
    use ddc_model::{MeasureAnchor, MeasureKind, MeasureSpec, ViewKey};
    let (mut m, lv, _) = house();
    for k in 1..=5 {
        let anchors = [MeasureAnchor::Point([0.0, 1.0, 0.0]), MeasureAnchor::Point([k as f64, 1.0, 0.0])];
        let spec = MeasureSpec {
            kind: MeasureKind::Distance,
            view: ViewKey::Plan(lv),
            pins: m.measure_pins(&anchors).unwrap(),
            label: String::new(),
        };
        m.add_measure(&spec).unwrap();
    }
    let s = m.add_sheet(&SheetDef::new("A-101", "Plan")).unwrap();
    m.add_viewport(&plan(s, lv, 100.0)).unwrap();
    let f = long(0);
    let c = compose_sheet(&m, s, &ctx(&f)).unwrap();
    let got = texts(&c.drawing, None);
    for k in 1..=5 {
        let want = format!("{}", k * 1000);
        assert!(got.iter().any(|t| *t == want), "{want} missing in {got:?}");
    }
    // the same numbers reach the PDF
    let pdf = PdfFile::parse(&to_pdf(&[c.drawing.to_pdf_sheet()]).unwrap()).unwrap();
    let pt: Vec<String> = pdf.page(0).unwrap().texts.iter().map(|t| t.string.clone()).collect();
    assert!(pt.iter().any(|t| t == "5000"), "{pt:?}");
    // a measurement without a number says why, never a zero
    assert_eq!(
        measure_text(None, "m", "orphan:1", LengthUnits::Metric),
        "n/a: orphan:1"
    );
}

#[test]
fn sheet_orphan_measure_says_why_never_zero() {
    use ddc_model::{DimRef, MeasureAnchor, MeasureKind, MeasureSpec, ViewKey};
    let (mut m, lv, _) = house();
    let ids: Vec<i64> = {
        let mut st = m.sql().prepare("SELECT id FROM wall WHERE level=?1 ORDER BY id").unwrap();
        st.query_map([lv], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
    };
    let anchors = [
        MeasureAnchor::Dim(DimRef::WallEnd { wall: ids[0], face: 1, end: 0 }),
        MeasureAnchor::Dim(DimRef::WallEnd { wall: ids[2], face: 0, end: 0 }),
    ];
    let spec = MeasureSpec {
        kind: MeasureKind::Distance,
        view: ViewKey::Plan(lv),
        pins: m.measure_pins(&anchors).unwrap(),
        label: String::new(),
    };
    m.add_measure(&spec).unwrap();
    m.delete_wall(ids[2]).unwrap();
    let s = m.add_sheet(&SheetDef::new("A-102", "Plan")).unwrap();
    m.add_viewport(&plan(s, lv, 100.0)).unwrap();
    let f = long(0);
    let c = compose_sheet(&m, s, &ctx(&f)).unwrap();
    let got = texts(&c.drawing, None);
    assert!(got.iter().any(|t| t.starts_with("n/a: orphan")), "{got:?}");
}
