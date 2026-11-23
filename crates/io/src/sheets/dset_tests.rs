// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use std::collections::BTreeMap;

use ddc_model::{DsetRef, GridSpec, LayerSpec, Model, WallSpec, DSET_LEVELS};
use serde_json::Value;

use super::dset::{flow_tables, page_key, page_table, plan_drawing_set, DsetOptions, Region};
use super::dset_check::check_set;
use super::dset_fixture::{building, typical_building, Made};
use super::set::{compose_sheet, export_sheets_pdf, Ctx};
use super::table::Table;
use super::LengthUnits;
use crate::dxf;
use crate::pdf::import::PdfFile;

fn catalog(lang: &str) -> BTreeMap<String, String> {
    let src = if lang == "ru" {
        [
            include_str!("../../../ui/data/i18n/ru/dset.json"),
            include_str!("../../../ui/data/i18n/ru/sheets_issue.json"),
        ]
    } else {
        [
            include_str!("../../../ui/data/i18n/en/dset.json"),
            include_str!("../../../ui/data/i18n/en/sheets_issue.json"),
        ]
    };
    let mut out = BTreeMap::new();
    for s in src {
        let v: Value = serde_json::from_str(s).unwrap();
        for (k, v) in v.as_object().unwrap() {
            out.insert(k.clone(), v.as_str().unwrap_or_default().to_string());
        }
    }
    out
}

fn schedule(key: &str, n: usize) -> Table {
    Table {
        title: key.to_string(),
        header: vec!["Mark".into(), "Name".into(), "Size".into(), "Count".into()],
        rows: (0..n)
            .map(|i| {
                vec![
                    format!("{key}{i}"),
                    "Item".into(),
                    "1500 x 1500".into(),
                    "1".into(),
                ]
            })
            .collect(),
        swatches: vec![],
    }
}

/// Model schedules as the editor gives them: one row an element.
fn tables(made: &Made) -> impl Fn(&str) -> Option<Table> {
    let (r, d, w) = (made.rooms, made.doors, made.windows);
    move |k: &str| match k {
        "rooms" => Some(schedule(k, r)),
        "doors" => Some(schedule(k, d)),
        "windows" => Some(schedule(k, w)),
        _ => None,
    }
}

struct Run {
    m: Model,
    made: Made,
    ids: Vec<i64>,
    paper: String,
    scale: f64,
}

fn run(lang: &str, floors: usize, opt: &DsetOptions) -> Run {
    let (mut m, made) = building(floors).unwrap();
    let cat = catalog(lang);
    let text = |k: &str| cat.get(k).cloned().unwrap_or_else(|| k.to_string());
    let sched = tables(&made);
    let ctx = Ctx {
        units: LengthUnits::Metric,
        schedules: &sched,
        index: None,
        paper: None,
        text: &text,
        refs: None,
    };
    let lv = super::dset::tag_levels(&m).unwrap();
    assert_eq!(lv.len(), floors);
    assert!(m.tag_for_drawings(&lv, &pfx()).unwrap() > 0);
    let p = plan_drawing_set(&m, opt, &ctx).unwrap();
    assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    let a = m.apply_drawing_set(&p.plan).unwrap();
    Run {
        m,
        made,
        ids: a.sheets,
        paper: p.paper,
        scale: p.plan_scale,
    }
}

fn ctx_for<'a>(
    text: &'a dyn Fn(&str) -> String,
    sched: &'a dyn Fn(&str) -> Option<Table>,
) -> Ctx<'a> {
    Ctx {
        units: LengthUnits::Metric,
        schedules: sched,
        index: None,
        paper: None,
        text,
        refs: None,
    }
}

#[test]
fn page_keys_and_parts() {
    assert_eq!(page_key("doors@3-9"), ("doors", Some(3..9)));
    assert_eq!(page_key("doors"), ("doors", None));
    assert_eq!(page_key("a@b"), ("a@b", None));
    let t = schedule("doors", 10);
    let p = page_table(t.clone(), Some(4..7), &|_| "cont.".into());
    assert_eq!(p.rows.len(), 3);
    assert_eq!(p.rows[0][0], "doors4");
    assert!(p.title.contains("cont."), "{}", p.title);
    assert_eq!(page_table(t, Some(0..3), &|_| "x".into()).title, "doors");
}

#[test]
fn long_schedules_flow_over_sheets_without_loss() {
    // six 50 mm columns of about 48 and 37 rows fill an A3; 500 rows need a second sheet
    let reg = Region::of("gost", "A3").unwrap();
    let flow = vec![
        ("rooms".to_string(), schedule("rooms", 300)),
        ("doors".to_string(), schedule("doors", 200)),
    ];
    let parts = flow_tables(&flow, &reg, &|k| k.to_string());
    assert!(parts.iter().any(|p| p.page > 0), "A3 cannot hold 500 rows");
    let mut rows = BTreeMap::new();
    for p in &parts {
        let (k, r) = page_key(&p.key);
        let r = r.unwrap_or(0..flow.iter().find(|f| f.0 == k).unwrap().1.rows.len());
        *rows.entry(k.to_string()).or_insert(0) += r.len();
        // every part inside the frame and clear of the title block
        let (x, y) = p.pos;
        assert!(
            y - p.h >= reg.frame[1] - 1e-6 && y <= reg.frame[3] + 1e-6,
            "{p:?}"
        );
        assert!(x >= reg.frame[0] - 1e-6, "{p:?}");
    }
    assert_eq!(rows["rooms"], 300);
    assert_eq!(rows["doors"], 200);
}

#[test]
fn three_storey_set_is_complete_and_clean() {
    let r = run("ru", 3, &DsetOptions::default());
    // cover + 3 plans + sections + elevations (+ schedule overflow)
    assert!(r.ids.len() >= 6, "{} sheets", r.ids.len());
    let cat = catalog("ru");
    let text = |k: &str| cat.get(k).cloned().unwrap_or_else(|| k.to_string());
    let sched = tables(&r.made);
    let ctx = ctx_for(&text, &sched);
    let rep = check_set(&r.m, &r.ids, &ctx).unwrap();
    assert!(rep.outside.is_empty(), "outside: {:?}", rep.outside);
    assert!(rep.overlaps.is_empty(), "overlaps: {:?}", rep.overlaps);
    assert_eq!(rep.tag_overlaps, 0);
    // 3 plans, 2 sections, 4 elevations
    assert_eq!(rep.views, 9, "{rep:?}");
    let has = |s: &str| rep.texts.iter().any(|t| t == s);
    let has_in = |s: &str| rep.texts.iter().any(|t| t.contains(s));
    // dimensions as built: bays 6000, windows 1500, overall on the faces or the axes
    assert!(has("6000") && has("1500"), "dims");
    assert!(has("18000") || has("18380"), "overall");
    // level marks of every storey on sections and elevations, cut materials hatched
    for (m, mm) in [("3", "300"), ("6", "600"), ("9", "900")] {
        let mark = |sep: &str| format!("+{m}{sep}{mm}");
        assert!(has(&mark(".")) || has(&mark(",")), "level mark +{m}.{mm}");
    }
    assert!(
        rep.level_marks >= 6 * 4,
        "{} level mark texts",
        rep.level_marks
    );
    assert!(rep.hatches > 0, "section cuts are hatched");
    // tags: room numbers and opening marks
    assert!(has_in("101") && has_in("W1") && has_in("D11"), "tags");
    // titles and the title block, in Russian
    let sec = "\u{0420}\u{0430}\u{0437}\u{0440}\u{0435}\u{0437}";
    assert!(
        has_in(&format!("{sec} 1-1")) && has_in(&format!("{sec} 2-2")),
        "sections"
    );
    let elev = "\u{0424}\u{0430}\u{0441}\u{0430}\u{0434} ";
    let elevs = rep.texts.iter().filter(|t| t.starts_with(elev)).count();
    assert!(
        elevs >= 4 && has_in("1\u{2013}4") && has_in("A\u{2013}C"),
        "elevations {elevs}"
    );
    let plan = "\u{041f}\u{043b}\u{0430}\u{043d}: Level 1";
    let first = "\u{0410}\u{0420}-1";
    let author = "\u{0420}\u{0430}\u{0437}\u{0440}\u{0430}\u{0431}.";
    assert!(has_in(plan) && has(first) && has(author), "stamp");
    assert!(
        has("Office building") && has("2026-11-OB"),
        "project fields"
    );
    // schedules: every element once across parts
    let mut rows = BTreeMap::new();
    for &id in &r.ids {
        for (_, p) in r.m.schedule_places(id).unwrap() {
            let (k, range) = page_key(&p.schedule);
            let n = match range {
                Some(x) => x.len(),
                None => sched(k).map(|t| t.rows.len()).unwrap_or(0),
            };
            *rows.entry(k.to_string()).or_insert(0) += n;
        }
    }
    assert_eq!(rows.get("rooms"), Some(&r.made.rooms));
    assert_eq!(rows.get("doors"), Some(&r.made.doors));
    assert_eq!(rows.get("windows"), Some(&r.made.windows));
    assert!((r.scale - 100.0).abs() < 1e-9, "1:{}", r.scale);
    assert!(!r.paper.is_empty());
}

/// Texts of one layer on a composed sheet.
fn layer_texts(c: &super::set::Composed, layer: usize) -> Vec<String> {
    c.drawing
        .marks
        .iter()
        .filter_map(|(_, m)| match m {
            super::Mark::Text { s, layer: l, .. } if *l == layer => Some(s.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn plans_carry_axes_both_sides_three_chains_and_floor_level() {
    let r = run("ru", 2, &DsetOptions::default());
    let cat = catalog("ru");
    let text = |k: &str| cat.get(k).cloned().unwrap_or_else(|| k.to_string());
    let sched = tables(&r.made);
    let ctx = ctx_for(&text, &sched);
    // sheets 2 and 3: plans of the ground and the first floor
    for (k, mark) in [(1, "\u{b1}0,000"), (2, "+3,300")] {
        let c = compose_sheet(&r.m, r.ids[k], &ctx).unwrap();
        // axis marks in circles at both ends of every axis
        let grid = layer_texts(&c, super::GRID);
        for l in ["1", "2", "3", "4", "A", "B", "C"] {
            let n = grid.iter().filter(|t| *t == l).count();
            assert_eq!(n, 2, "sheet {k}: axis {l} marked {n} times");
        }
        // three chains outside: openings, axis to axis, overall, on all four sides
        let dims = layer_texts(&c, super::DIMS);
        let bays = dims.iter().filter(|t| *t == "6000").count();
        assert!(bays >= 3 * 2 + 2 * 2, "sheet {k}: {bays} bay sizes");
        let overall = |s: &str| dims.iter().filter(|t| *t == s).count();
        assert!(
            overall("18000") + overall("18380") >= 2,
            "sheet {k}: overall x"
        );
        assert!(
            overall("12000") + overall("12380") >= 2,
            "sheet {k}: overall y"
        );
        assert!(dims.iter().any(|t| t == "1500"), "sheet {k}: opening chain");
        // finished floor level relative to +-0.000, in its frame beside the title
        let lv = layer_texts(&c, super::LEVELS);
        assert!(lv.iter().any(|t| t == mark), "sheet {k}: {lv:?}");
    }
}

#[test]
fn set_prints_to_pdf_and_dxf() {
    let r = run("en", 3, &DsetOptions::default());
    let cat = catalog("en");
    let text = |k: &str| cat.get(k).cloned().unwrap_or_else(|| k.to_string());
    let sched = tables(&r.made);
    let ctx = ctx_for(&text, &sched);
    let pdf = export_sheets_pdf(&r.m, &r.ids, &ctx).unwrap();
    if let Ok(dir) = std::env::var("DDC_DSET_OUT") {
        std::fs::write(format!("{dir}/dset_en.pdf"), &pdf).unwrap();
    }
    let f = PdfFile::parse(&pdf).unwrap();
    assert_eq!(f.page_count(), r.ids.len());
    let p0 = f.page(0).unwrap();
    let all: Vec<&str> = p0.texts.iter().map(|t| t.string.as_str()).collect();
    assert!(all.iter().any(|s| s.contains("A-1")), "{all:?}");
    assert!(all.iter().any(|s| s.contains("Sheet list")), "{all:?}");
    let p1 = f.page(1).unwrap();
    assert!(p1.texts.iter().any(|t| t.string.contains("Level 1 plan")));
    for &id in &r.ids[..2] {
        let c = compose_sheet(&r.m, id, &ctx).unwrap();
        let s = dxf::write_string(&c.drawing.to_dxf());
        let doc = dxf::read_str(&s).unwrap();
        // views in model space, the title block and the sheet list in paper space
        let paper = doc
            .blocks
            .iter()
            .find(|b| b.name.eq_ignore_ascii_case("*Paper_Space"))
            .expect("paper space");
        let texts: Vec<&str> = doc
            .entities
            .iter()
            .chain(&paper.entities)
            .filter_map(|e| match e {
                dxf::Entity::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect();
        assert!(texts.len() > 10, "{} texts", texts.len());
        assert!(texts.iter().any(|t| t.contains("A-")), "{texts:?}");
    }
}

/// Marks by type, tags and the set run inside an outer transaction (an AI plan's Apply, the
/// command's step), as one undo step; a mark typed by hand is never touched.
#[test]
fn set_runs_inside_an_outer_atomic_step() {
    let (mut m, made) = super::dset_fixture::unmarked_building(2).unwrap();
    let wins = openings(&m, "window");
    let hand = ddc_model::TagHost::Opening(wins[0]);
    m.set_mark(hand, "W7").unwrap();
    let lv = super::dset::tag_levels(&m).unwrap();
    let unmarked = m.unmarked_openings(&lv).unwrap();
    assert_eq!(unmarked, made.doors + made.windows - 1);
    let cat = catalog("en");
    let text = |k: &str| cat.get(k).cloned().unwrap_or_else(|| k.to_string());
    let sched = tables(&made);
    let before = m.sheets().unwrap().len();
    let n = m
        .atomic("outer", |m| {
            let lv = super::dset::tag_levels(m).map_err(|e| ddc_model::Error::Invalid(e.0))?;
            m.tag_for_drawings(&lv, &pfx())?;
            let ctx = ctx_for(&text, &sched);
            let p = plan_drawing_set(m, &DsetOptions::default(), &ctx)
                .map_err(|e| ddc_model::Error::Invalid(e.0))?;
            Ok(m.apply_drawing_set(&p.plan)?.sheets.len())
        })
        .unwrap();
    assert!(n >= 5);
    assert_eq!(m.unmarked_openings(&lv).unwrap(), 0);
    assert_eq!(m.mark(hand).unwrap(), "W7");
    m.undo().unwrap();
    assert_eq!(m.sheets().unwrap().len(), before, "one undo step");
    assert!(
        m.tags(made.levels[0]).unwrap().is_empty(),
        "tags undone too"
    );
    assert_eq!(
        m.unmarked_openings(&lv).unwrap(),
        unmarked,
        "marks undone too"
    );
    assert_eq!(m.mark(hand).unwrap(), "W7");
}

#[test]
fn regeneration_keeps_the_sheets() {
    let mut r = run("en", 2, &DsetOptions::default());
    let first = r.ids.clone();
    let cat = catalog("en");
    let text = |k: &str| cat.get(k).cloned().unwrap_or_else(|| k.to_string());
    let sched = tables(&r.made);
    let ctx = ctx_for(&text, &sched);
    let p = plan_drawing_set(&r.m, &DsetOptions::default(), &ctx).unwrap();
    let again = r.m.apply_drawing_set(&p.plan).unwrap();
    assert_eq!(again.sheets, first);
    let views = r.m.view_defs().unwrap().len();
    // a change to the model: one more storey on top
    let lv = r.made.levels[2];
    let wt = r.made.wall_type;
    r.m.add_walls(&[ddc_model::WallSpec {
        a: (0.0, 0.0),
        b: (6.0, 0.0),
        level: lv,
        base_offset: 0.0,
        height: 3.0,
        wall_type: wt,
        offset0: None,
    }])
    .unwrap();
    let p = plan_drawing_set(&r.m, &DsetOptions::default(), &ctx).unwrap();
    let third = r.m.apply_drawing_set(&p.plan).unwrap();
    assert_eq!(third.sheets[0], first[0], "cover kept");
    assert_eq!(&third.sheets[1..3], &first[1..3], "plans kept");
    assert_eq!(third.sheets.len(), first.len() + 1);
    assert_eq!(r.m.view_defs().unwrap().len(), views, "views reused");
    assert_eq!(r.m.drawing_set_sheets().unwrap().len(), third.sheets.len());
}

#[test]
fn options_pick_sheets_paper_and_iso() {
    let opt = DsetOptions {
        sections: false,
        elevations: false,
        schedules: false,
        scale: Some(200.0),
        paper: Some("A2".into()),
        standard: "iso".into(),
        ..Default::default()
    };
    let r = run("en", 2, &opt);
    // cover with the sheet list, two plans
    assert_eq!(r.ids.len(), 3);
    assert_eq!(r.paper, "A2");
    for &id in &r.ids {
        let s = r.m.sheet(id).unwrap();
        assert_eq!((s.paper.as_str(), s.standard.as_str()), ("A2", "iso"));
    }
    let vps = r.m.viewports(r.ids[1]).unwrap();
    assert_eq!(vps.len(), 1);
    assert!((vps[0].1.scale - 200.0).abs() < 1e-9);
}

/// Mark prefixes of the test sets.
fn pfx() -> ddc_model::MarkPrefix {
    ddc_model::MarkPrefix {
        door: "D".into(),
        window: "W".into(),
    }
}

fn floor_mark(z: f64) -> String {
    if z.abs() < 1e-9 {
        "\u{b1}0,000".into()
    } else {
        format!("+{z:.3}").replace('.', ",")
    }
}

/// Levels each plan sheet stands for: the typical floor field, else its viewport's level.
fn plan_levels(s: &ddc_model::DsetSheet) -> Vec<i64> {
    match s.def.fields.get(DSET_LEVELS) {
        Some(v) => v.split(',').map(|x| x.parse().unwrap()).collect(),
        None => s
            .viewports
            .iter()
            .filter_map(|v| match v.view {
                DsetRef::Plan { level, .. } => Some(level),
                _ => None,
            })
            .collect(),
    }
}

/// Floors that draw alike share one typical floor plan sheet, titled by its level range and
/// marked with every floor level it stands for; each floor is on exactly one plan.
#[test]
fn alike_floors_make_one_typical_floor_plan() {
    let (mut m, made) = typical_building(5).unwrap();
    let cat = catalog("en");
    let text = |k: &str| cat.get(k).cloned().unwrap_or_else(|| k.to_string());
    let sched = tables(&made);
    let ctx = ctx_for(&text, &sched);
    let lv = super::dset::tag_levels(&m).unwrap();
    m.tag_for_drawings(&lv, &pfx()).unwrap();
    let p = plan_drawing_set(&m, &DsetOptions::default(), &ctx).unwrap();
    assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    let plans: Vec<&ddc_model::DsetSheet> = p
        .plan
        .sheets
        .iter()
        .filter(|s| s.key.starts_with("plan:"))
        .collect();
    assert!(
        plans.len() < 5,
        "{} plan sheets for 5 alike floors",
        plans.len()
    );
    let mut covered: Vec<i64> = plans.iter().flat_map(|s| plan_levels(s)).collect();
    covered.sort();
    assert_eq!(covered, made.levels[..5].to_vec());
    let typ: Vec<&&ddc_model::DsetSheet> = plans
        .iter()
        .filter(|s| s.def.fields.contains_key(DSET_LEVELS))
        .collect();
    assert_eq!(typ.len(), 1, "one typical floor");
    let t = typ[0];
    let ids = plan_levels(t);
    assert!(ids.len() >= 2);
    assert_eq!(t.key, format!("plan:{}", ids[0]));
    let first = made.levels.iter().position(|&l| l == ids[0]).unwrap() + 1;
    let last = made
        .levels
        .iter()
        .position(|&l| l == ids[ids.len() - 1])
        .unwrap()
        + 1;
    let want = format!("Typical floor plan (Level {first}\u{2013}{last})");
    assert_eq!(t.def.name, want);
    // applied and drawn: the typical plan marks every floor level it stands for
    let a = m.apply_drawing_set(&p.plan).unwrap();
    let k = p.plan.sheets.iter().position(|s| s.key == t.key).unwrap();
    let c = compose_sheet(&m, a.sheets[k], &ctx).unwrap();
    let marks = layer_texts(&c, super::LEVELS);
    for id in &ids {
        let i = made.levels.iter().position(|l| l == id).unwrap();
        let z = i as f64 * super::dset_fixture::STOREY;
        assert!(
            marks.contains(&floor_mark(z)),
            "{} not in {marks:?}",
            floor_mark(z)
        );
    }
    let rep = check_set(&m, &a.sheets, &ctx).unwrap();
    assert!(rep.outside.is_empty(), "outside: {:?}", rep.outside);
    assert!(rep.overlaps.is_empty(), "overlaps: {:?}", rep.overlaps);
    assert_eq!(rep.tag_overlaps, 0, "{rep:?}");
}

/// A typical floor plan shows no room number of one floor only: the part every floor
/// shares (".01") and a note that reads it back to the first and the last floor's numbers.
#[test]
fn typical_floor_room_numbers_hold_for_every_floor() {
    for lang in ["en", "ru"] {
        let (mut m, made) = typical_building(5).unwrap();
        let cat = catalog(lang);
        let text = |k: &str| cat.get(k).cloned().unwrap_or_else(|| k.to_string());
        let sched = tables(&made);
        let ctx = ctx_for(&text, &sched);
        let lv = super::dset::tag_levels(&m).unwrap();
        m.tag_for_drawings(&lv, &pfx()).unwrap();
        let p = plan_drawing_set(&m, &DsetOptions::default(), &ctx).unwrap();
        let k = p
            .plan
            .sheets
            .iter()
            .position(|s| s.def.fields.contains_key(DSET_LEVELS))
            .expect("a typical floor");
        let ids = plan_levels(&p.plan.sheets[k]);
        let a = m.apply_drawing_set(&p.plan).unwrap();
        let c = compose_sheet(&m, a.sheets[k], &ctx).unwrap();
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
        let numbers = |lv: i64| -> Vec<String> {
            let mut q = m
                .sql()
                .prepare("SELECT number FROM room WHERE level=?1 ORDER BY number")
                .unwrap();
            q.query_map([lv], |r| r.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        let all: Vec<String> = ids.iter().flat_map(|&l| numbers(l)).collect();
        assert!(all.len() >= 2 * made.rooms / 5, "{all:?}");
        let words: Vec<String> = layer_texts(&c, super::TAGS)
            .iter()
            .flat_map(|t| t.split_whitespace().map(str::to_string).collect::<Vec<_>>())
            .collect();
        for n in &all {
            assert!(
                !words.contains(n),
                "{lang}: room number {n} of one floor on the plan"
            );
        }
        assert!(words.iter().any(|w| w == ".01"), "{lang}: {words:?}");
        let (first, last) = (numbers(ids[0]), numbers(ids[ids.len() - 1]));
        let note = text("dset.note.room_numbers");
        let head = note.split('{').next().unwrap();
        let titles = layer_texts(&c, super::TITLES);
        let line = titles
            .iter()
            .find(|t| t.starts_with(head))
            .unwrap_or_else(|| panic!("{lang}: no room note in {titles:?}"));
        let names = |ns: &[String]| ns.iter().any(|n| line.contains(n.as_str()));
        assert!(names(&first) && names(&last), "{line}");
        let rep = check_set(&m, &a.sheets, &ctx).unwrap();
        assert!(rep.outside.is_empty(), "outside: {:?}", rep.outside);
        assert!(rep.overlaps.is_empty(), "overlaps: {:?}", rep.overlaps);
    }
}

/// Ids of the openings of `kind`, in the order the fixture made them.
fn openings(m: &Model, kind: &str) -> Vec<i64> {
    let mut st = m
        .sql()
        .prepare("SELECT id FROM opening WHERE kind=?1 ORDER BY id")
        .unwrap();
    let ids = st.query_map([kind], |r| r.get(0)).unwrap();
    ids.map(Result::unwrap).collect()
}

/// Doors and windows without a mark take their type's mark on the levels a set tags: a type
/// marked by hand keeps that mark, others the prefix and the first free number.
#[test]
fn unmarked_openings_take_their_type_mark() {
    let (mut m, made) = super::dset_fixture::unmarked_building(2).unwrap();
    let (doors, wins) = (openings(&m, "door"), openings(&m, "window"));
    m.set_mark(ddc_model::TagHost::Opening(wins[0]), "W7")
        .unwrap();
    let mark = |m: &Model, id: i64| m.mark(ddc_model::TagHost::Opening(id)).unwrap();
    m.tag_for_drawings(&made.levels[..1], &pfx()).unwrap();
    // the fixture's first floor: 10 windows and 7 doors, each of one size
    assert!(wins[..10].iter().all(|&w| mark(&m, w) == "W7"));
    assert!(doors[..7].iter().all(|&d| mark(&m, d) == "D1"));
    assert!(
        wins[10..].iter().all(|&w| mark(&m, w).is_empty()),
        "other levels stay"
    );
    let lv = made.levels[..1].to_vec();
    assert_eq!(m.mark_by_type(&lv, &pfx()).unwrap(), 0);
}

/// An unmarked building's set: no tag text on another text of a view (section heads beyond
/// the dimension chains, tags clear of dimension figures, axes and areas), no tag without
/// a mark, and room areas in the unit of the sheet's language.
#[test]
fn set_texts_stay_clear_of_each_other() {
    let (en_unit, ru_unit) = ("m\u{b2}", "\u{43c}\u{b2}");
    for (lang, unit, other) in [("en", en_unit, ru_unit), ("ru", ru_unit, en_unit)] {
        let (mut m, made) = super::dset_fixture::unmarked_building(3).unwrap();
        let cat = catalog(lang);
        let text = |k: &str| cat.get(k).cloned().unwrap_or_else(|| k.to_string());
        let sched = tables(&made);
        let ctx = ctx_for(&text, &sched);
        let prefix = super::dset::mark_prefix(&text);
        let lv = super::dset::tag_levels(&m).unwrap();
        m.tag_for_drawings(&lv, &prefix).unwrap();
        let p = plan_drawing_set(&m, &DsetOptions::default(), &ctx).unwrap();
        let ids = m.apply_drawing_set(&p.plan).unwrap().sheets;
        let rep = check_set(&m, &ids, &ctx).unwrap();
        assert_eq!(rep.tag_overlaps, 0, "{lang}: {:?}", rep.tag_pairs);
        assert!(rep.outside.is_empty(), "{lang}: {:?}", rep.outside);
        assert!(rep.overlaps.is_empty(), "{lang}: {:?}", rep.overlaps);
        assert!(
            !rep.texts.iter().any(|t| t == "?"),
            "{lang}: a tag without a mark"
        );
        let (door, win) = (format!("{}1", prefix.door), format!("{}1", prefix.window));
        assert!(
            rep.texts.contains(&door) && rep.texts.contains(&win),
            "{lang}"
        );
        assert!(rep.texts.iter().any(|t| t.ends_with(unit)), "{lang}: areas");
        assert!(
            !rep.texts.iter().any(|t| t.contains(other)),
            "{lang}: area unit"
        );
    }
}

/// A 40 storey building: the plans stay at 1:100 on their paper while the tall sections and
/// elevations alone go coarser; the middle floors are one typical floor plan.
#[test]
fn a_tall_building_keeps_its_plans_at_one_to_one_hundred() {
    let (mut m, made) = typical_building(40).unwrap();
    let cat = catalog("ru");
    let text = |k: &str| cat.get(k).cloned().unwrap_or_else(|| k.to_string());
    let sched = tables(&made);
    let ctx = ctx_for(&text, &sched);
    let lv = super::dset::tag_levels(&m).unwrap();
    m.tag_for_drawings(&lv, &pfx()).unwrap();
    let p = plan_drawing_set(&m, &DsetOptions::default(), &ctx).unwrap();
    assert!(
        (p.plan_scale - 100.0).abs() < 1e-9,
        "plans 1:{}",
        p.plan_scale
    );
    assert!(p.view_scale > p.plan_scale, "views 1:{}", p.view_scale);
    let plans: Vec<&ddc_model::DsetSheet> = p
        .plan
        .sheets
        .iter()
        .filter(|s| s.key.starts_with("plan:"))
        .collect();
    assert!(plans.len() <= 4, "{} plan sheets", plans.len());
    let longest = plans.iter().map(|s| plan_levels(s).len()).max().unwrap();
    assert!(longest >= 37, "typical floor of {longest} levels");
    let typical = "\u{041f}\u{043b}\u{0430}\u{043d} \u{0442}\u{0438}\u{043f}\u{043e}\u{0432}";
    assert!(plans.iter().any(|s| s.def.name.starts_with(typical)));
    for s in &p.plan.sheets {
        for v in &s.viewports {
            let want = match v.view {
                DsetRef::Plan { .. } => p.plan_scale,
                _ => p.view_scale,
            };
            assert!((v.scale - want).abs() < 1e-9, "{}: 1:{}", v.title, v.scale);
        }
    }
    let a = m.apply_drawing_set(&p.plan).unwrap();
    let rep = check_set(&m, &a.sheets, &ctx).unwrap();
    assert!(rep.outside.is_empty(), "outside: {:?}", rep.outside);
    assert!(rep.overlaps.is_empty(), "overlaps: {:?}", rep.overlaps);
    assert_eq!(rep.tag_overlaps, 0, "{rep:?}");
}

#[test]
fn level_ranges_name_the_shared_words_once() {
    use super::dset::level_range;
    assert_eq!(level_range("Level 2", "Level 40"), "Level 2\u{2013}40");
    assert_eq!(level_range("Level 12", "Level 13"), "Level 12\u{2013}13");
    let floor = "\u{042d}\u{0442}\u{0430}\u{0436} ";
    assert_eq!(
        level_range(&format!("{floor}2"), &format!("{floor}40")),
        format!("{floor}2\u{2013}40")
    );
    assert_eq!(level_range("Ground", "Roof"), "Ground \u{2013} Roof");
}

/// Axes lettered in Cyrillic (GOST 21.101): elevations read by their end axes, as seen.
#[test]
fn elevations_are_titled_by_cyrillic_end_axes() {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("1", 0.0).unwrap();
    m.add_level("2", 3.3).unwrap();
    let layer = LayerSpec {
        material: "brick".into(),
        thickness: 0.38,
    };
    let wt = m.add_wall_type("Brick 380", &[layer]).unwrap();
    let pts = [(0.0, 0.0), (18.0, 0.0), (18.0, 12.0), (0.0, 12.0)];
    let specs: Vec<WallSpec> = (0..4)
        .map(|k| WallSpec {
            a: pts[k],
            b: pts[(k + 1) % 4],
            level: lv,
            base_offset: 0.0,
            height: 3.3,
            wall_type: wt,
            offset0: None,
        })
        .collect();
    m.add_walls(&specs).unwrap();
    for (n, x) in ["1", "2", "3", "4"].iter().zip([0.0, 6.0, 12.0, 18.0]) {
        m.add_grid(&GridSpec::line(n, (x, -2.5), (x, 14.5)))
            .unwrap();
    }
    let rows = ["\u{0410}", "\u{0411}", "\u{0412}", "\u{0413}"];
    for (n, y) in rows.iter().zip([0.0, 4.0, 8.0, 12.0]) {
        m.add_grid(&GridSpec::line(n, (-2.5, y), (20.5, y)))
            .unwrap();
    }
    let cat = catalog("ru");
    let text = |k: &str| cat.get(k).cloned().unwrap_or_else(|| k.to_string());
    let sched = |_: &str| None::<Table>;
    let ctx = ctx_for(&text, &sched);
    let opt = DsetOptions {
        sections: false,
        schedules: false,
        ..Default::default()
    };
    let p = plan_drawing_set(&m, &opt, &ctx).unwrap();
    let elev = "\u{0424}\u{0430}\u{0441}\u{0430}\u{0434} ";
    let titles: Vec<String> = p
        .plan
        .sheets
        .iter()
        .flat_map(|s| s.viewports.iter().map(|v| v.title.clone()))
        .filter(|t| t.starts_with(elev))
        .collect();
    assert_eq!(titles.len(), 4, "{titles:?}");
    let (a, g) = (rows[0], rows[3]);
    for ends in [
        "1\u{2013}4".to_string(),
        "4\u{2013}1".to_string(),
        format!("{a}\u{2013}{g}"),
        format!("{g}\u{2013}{a}"),
    ] {
        assert!(
            titles.contains(&format!("{elev}{ends}")),
            "{ends}: {titles:?}"
        );
    }
}

// ---- documentation: internal dimensions, area and finishes schedules, the whole set ----

/// Texts of the layers of one composed sheet as (rect, string).
fn text_rects(c: &super::set::Composed, layers: &[usize]) -> Vec<([f64; 4], String)> {
    c.drawing
        .marks
        .iter()
        .filter_map(|(_, m)| match m {
            super::Mark::Text { pos, h, rot, s, layer } if layers.contains(layer) => {
                Some((super::annot::text_rect(*pos, *h, *rot, s), s.clone()))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn plans_dimension_partitions_and_rooms_and_no_text_overlaps() {
    let r = run("en", 2, &DsetOptions::default());
    let cat = catalog("en");
    let text = |k: &str| cat.get(k).cloned().unwrap_or_else(|| k.to_string());
    let sched = tables(&r.made);
    let ctx = ctx_for(&text, &sched);
    for (k, &lv) in r.made.levels[..2].iter().enumerate() {
        // the model gives internal chains on every plan: the partition openings and the rooms
        let v = r
            .m
            .plan_view(&ddc_model::PlanViewSpec { level: lv, cut_height: 1.2, grids: vec![] })
            .unwrap();
        let inner: Vec<f64> = v
            .prims
            .iter()
            .filter_map(|p| match p {
                ddc_model::ViewPrim::Dim { chain, value, .. } if *chain >= ddc_model::INNER_CHAIN => {
                    Some(*value)
                }
                _ => None,
            })
            .collect();
        let doors = inner.iter().filter(|v| (**v - 0.9).abs() < 1e-9).count();
        assert_eq!(doors, 7, "level {k}: one size per partition door");
        // six rooms, each crossed once each way
        let clear = inner.iter().filter(|v| **v > 5.7).count();
        assert!(clear >= 12, "level {k}: {clear} clear room sizes in {inner:?}");

        // on the sheet: the sizes are there and no two texts of dimensions and tags touch
        let c = compose_sheet(&r.m, r.ids[k + 1], &ctx).unwrap();
        let ts = text_rects(&c, &[super::DIMS, super::TAGS]);
        let sizes = |s: &str| ts.iter().filter(|t| t.1 == s).count();
        assert!(sizes("900") >= 7, "sheet {k}: {} door sizes drawn", sizes("900"));
        assert!(sizes("5750") >= 4, "sheet {k}: room sizes");
        // the tag of every room names its finish at the plan scale of the set
        let carpet = ts.iter().filter(|t| t.1 == "Carpet tile").count();
        assert!(carpet >= 5, "sheet {k}: {carpet} finishes on tags");
        let rects: Vec<[f64; 4]> = ts.iter().map(|t| t.0).collect();
        let over = ddc_geom::label::total_overlap(&rects);
        assert!(over < 1e-6, "sheet {k}: texts overlap by {over} mm2");
    }
}

fn number(cell: &str) -> f64 {
    cell.replace(',', ".").parse().unwrap_or(f64::NAN)
}

#[test]
fn area_schedule_equals_the_model_areas_by_each_method() {
    let floors = 3;
    let (mut m, made) = building(floors).unwrap();
    let cat = catalog("en");
    let text = |k: &str| cat.get(k).cloned().unwrap_or_else(|| k.to_string());
    // rooms as the model measures them: the stair hall of each storey and five offices
    let mut hall = 0.0;
    let mut offices = 0.0;
    for lv in &made.levels[..floors] {
        let ids: Vec<i64> = m
            .plan_view(&ddc_model::PlanViewSpec { level: *lv, cut_height: 1.2, grids: vec![] })
            .unwrap()
            .prims
            .iter()
            .filter_map(|p| match p {
                ddc_model::ViewPrim::RoomTag { room, .. } => Some(*room),
                _ => None,
            })
            .collect();
        assert_eq!(ids.len(), 6);
        for (n, id) in ids.iter().enumerate() {
            let a = m.room_quantities(*id).unwrap().unwrap().area;
            if n == 0 { hall += a } else { offices += a }
        }
    }
    let net = hall + offices;
    // outer walls 0.38 on the 18 x 12 rectangle
    let (outer, inner) = (18.38 * 12.38, 17.62 * 11.62);
    for (pack, want) in [
        ("generic", net),
        ("ru", net),
        ("de", offices),
        ("uk", floors as f64 * inner),
        ("us", floors as f64 * outer),
    ] {
        m.set_norm_locale(&ddc_model::checks::NormLocale { pack: pack.into(), ..Default::default() })
            .unwrap();
        let t = super::dset_area::areas(&m, &text).unwrap();
        let last = t.rows.last().unwrap();
        let got = number(&last[4]);
        assert!((got - want).abs() < 0.006 * floors as f64, "{pack}: table {got} vs model {want}");
        // every storey has its rows and a storey total
        for lv in 1..=floors {
            let name = format!("Level {lv}");
            assert!(
                t.rows.iter().any(|r| r[0] == name && r[1] == "Storey total"),
                "{pack}: no total for {name}"
            );
        }
        assert!(t.title.contains(pack), "{}", t.title);
    }
}

#[test]
fn drawing_set_has_every_storey_section_elevation_and_the_new_schedules() {
    let r = run("en", 3, &DsetOptions::default());
    let names: Vec<String> = r.m.sheets().unwrap().into_iter().map(|s| s.1.name).collect();
    for lv in 1..=3 {
        assert!(
            names.iter().any(|n| n.contains(&format!("Level {lv} plan"))),
            "no plan of Level {lv}: {names:?}"
        );
    }
    assert!(names.iter().any(|n| n.contains("Section")), "{names:?}");
    assert!(names.iter().any(|n| n.contains("elevation") || n.contains("Elevation")), "{names:?}");
    let cat = catalog("en");
    let text = |k: &str| cat.get(k).cloned().unwrap_or_else(|| k.to_string());
    let areas = super::dset_area::areas(&r.m, &text).unwrap();
    let fin = super::dset_area::finishes(&r.m, &text).unwrap();
    assert_eq!(fin.rows.len(), r.made.rooms, "a finishes row per room");
    // every row of both schedules is on some sheet, once
    let mut rows = BTreeMap::new();
    for &id in &r.ids {
        for (_, p) in r.m.schedule_places(id).unwrap() {
            let (k, range) = page_key(&p.schedule);
            let whole = match k {
                "areas" => areas.rows.len(),
                "finishes" => fin.rows.len(),
                _ => 0,
            };
            *rows.entry(k.to_string()).or_insert(0) += range.map_or(whole, |x| x.len());
        }
    }
    assert_eq!(rows.get("areas"), Some(&areas.rows.len()), "{rows:?}");
    assert_eq!(rows.get("finishes"), Some(&fin.rows.len()), "{rows:?}");
}

fn form(m: &mut Model, lv: i64, r: [f64; 4], z: (f64, f64)) {
    use ddc_geom::v2;
    use ddc_model::{SolidForm, SolidFrame, SolidSection, SolidSpec};
    let ring = vec![v2(r[0], r[1]), v2(r[2], r[1]), v2(r[2], r[3]), v2(r[0], r[3])];
    m.add_solid(&SolidSpec {
        level: lv,
        name: "form".into(),
        void: false,
        material: "stone".into(),
        ifc: None,
        frame: SolidFrame::default(),
        form: SolidForm::Extrusion { profile: SolidSection::polygon(ring), start: z.0, end: z.1 },
    })
    .unwrap();
}

#[test]
fn a_model_of_forms_alone_gets_plan_sections_and_elevations() {
    // a bridge-like model: piers, a deck and parapets, one level, no walls or rooms
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("Level 1", 0.0).unwrap();
    for x in [0.0, 14.0, 28.0] {
        form(&mut m, lv, [x, 0.0, x + 2.0, 6.0], (0.0, 4.0));
    }
    form(&mut m, lv, [0.0, 0.0, 30.0, 6.0], (4.0, 5.0));
    for y in [0.0, 5.7] {
        form(&mut m, lv, [0.0, y, 30.0, y + 0.3], (5.0, 6.1));
    }
    let cat = catalog("en");
    let text = |k: &str| cat.get(k).cloned().unwrap_or_else(|| k.to_string());
    let sched = |_: &str| None;
    let ctx = ctx_for(&text, &sched);
    let p = plan_drawing_set(&m, &DsetOptions::default(), &ctx).unwrap();
    let vps: Vec<&ddc_model::DsetViewport> =
        p.plan.sheets.iter().flat_map(|s| &s.viewports).collect();
    assert!(vps.iter().any(|v| v.view == DsetRef::Plan { level: lv, cut: ddc_model::DEFAULT_CUT }));
    let kinds: Vec<ddc_model::ViewKind> = vps
        .iter()
        .filter_map(|v| match v.view {
            DsetRef::Def(k) => Some(p.plan.views[k].kind),
            _ => None,
        })
        .collect();
    assert!(kinds.contains(&ddc_model::ViewKind::Section), "{kinds:?}");
    assert!(kinds.contains(&ddc_model::ViewKind::Elevation), "{kinds:?}");
    // the views span the whole model, not a wall extent
    for s in p.plan.views.iter().filter(|s| s.kind == ddc_model::ViewKind::Elevation) {
        let len = ((s.b.0 - s.a.0).powi(2) + (s.b.1 - s.a.1).powi(2)).sqrt();
        assert!(len >= 6.0, "{} spans {len}", s.name);
    }
    let a = m.apply_drawing_set(&p.plan).unwrap();
    assert!(!a.sheets.is_empty());
}

#[test]
fn a_model_with_nothing_to_draw_says_what_to_add() {
    let mut m = Model::open(":memory:").unwrap();
    m.add_level("Level 1", 0.0).unwrap();
    let cat = catalog("en");
    let text = |k: &str| cat.get(k).cloned().unwrap_or_else(|| k.to_string());
    let sched = |_: &str| None;
    let e = plan_drawing_set(&m, &DsetOptions::default(), &ctx_for(&text, &sched)).unwrap_err();
    assert!(e.0.contains("place elements on a level"), "{}", e.0);
}
